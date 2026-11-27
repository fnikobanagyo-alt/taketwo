//! Studio restoration (step S18): backend/app/studio.py `restore`, line for line. The model (`_restore_segment`)
//! comes in through the `Restorer` trait; this file only resamples, filters, windows and cross-fades.

use crate::audio::{self, PyError};
use crate::render::models::Restorer;
use crate::render::{RenderError, CANCELLED};
use std::sync::atomic::{AtomicBool, Ordering};

/// studio.py `SR_IN`, `SR_OUT`, `CHUNK_S`, `OVERLAP_S`.
pub const SR_IN: u32 = 16000;
pub const SR_OUT: u32 = 48000;
const CHUNK_S: f64 = 20.0;
const OVERLAP_S: f64 = 1.0;

fn model_err(e: String, cancel: &AtomicBool) -> RenderError {
    if e == CANCELLED || cancel.load(Ordering::SeqCst) { RenderError::Cancelled } else { RenderError::Other(e) }
}

/// `np.linspace(a, b, n)` cast to float32.
fn linspace(a: f64, b: f64, n: usize) -> Vec<f32> {
    if n == 1 {
        return vec![a as f32];
    }
    let step = (b - a) / (n - 1) as f64;
    let mut v: Vec<f32> = (0..n).map(|i| (a + i as f64 * step) as f32).collect();
    v[n - 1] = b as f32; // numpy sets the end point exactly
    v
}

/// `restore(a, sr, progress)`: 48 kHz audio (the rate is always `SR_OUT`). Long audio runs in 20 s windows with
/// a 1 s cross-faded overlap; the cancel flag is checked before each window.
pub fn restore(a: &[f32], sr: u32, r: &mut dyn Restorer, progress: &mut dyn FnMut(f64), cancel: &AtomicBool) -> Result<Vec<f32>, RenderError> {
    let x = audio::highpass(&audio::resample(a, sr, SR_IN), SR_IN, 50.0).map_err(|e: PyError| RenderError::Other(format!("{}: {}", e.kind, e.message)))?;
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let peak = if peak == 0.0 { 1.0 } else { peak };
    let x: Vec<f32> = x.iter().map(|v| v / peak * 0.9).collect();
    let (n, step, ov) = (x.len(), (CHUNK_S * SR_IN as f64) as usize, (OVERLAP_S * SR_IN as f64) as usize);
    if n <= step + ov {
        if cancel.load(Ordering::SeqCst) {
            return Err(RenderError::Cancelled);
        }
        return r.restore_segment(&x).map_err(|e| model_err(e, cancel));
    }
    let mut y = vec![0.0f32; n * 3];
    let mut w = vec![0.0f32; n * 3];
    let starts: Vec<usize> = (0..n).step_by(step).collect();
    for (k, &s) in starts.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(RenderError::Cancelled);
        }
        let lo = s.saturating_sub(ov);
        let seg = &x[lo..n.min(s + step + ov)];
        let out = r.restore_segment(seg).map_err(|e| model_err(e, cancel))?;
        let o0 = lo * 3;
        let rr = ov * 3;
        if out.len() < rr || o0 + out.len() > y.len() {
            // numpy would fail to broadcast here
            return Err(RenderError::Other(format!("ValueError: the restored window has {} samples, expected about {}", out.len(), seg.len() * 3)));
        }
        let mut ramp = vec![1.0f32; out.len()];
        if s > 0 {
            ramp[..rr].copy_from_slice(&linspace(0.0, 1.0, rr));
        }
        if s + step + ov < n {
            let l = ramp.len();
            ramp[l - rr..].copy_from_slice(&linspace(1.0, 0.0, rr));
        }
        for i in 0..out.len() {
            y[o0 + i] += out[i] * ramp[i];
            w[o0 + i] += ramp[i];
        }
        progress((k + 1) as f64 / starts.len() as f64);
    }
    Ok(y.iter().zip(&w).map(|(v, wt)| v / wt.max(1e-4)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Repeats each sample three times; records the segment lengths.
    struct Triple(Vec<usize>, bool);

    impl Restorer for Triple {
        fn restore_segment(&mut self, x16: &[f32]) -> Result<Vec<f32>, String> {
            self.0.push(x16.len());
            Ok(x16.iter().flat_map(|v| [*v; 3]).collect())
        }
        fn available(&self) -> bool {
            self.1
        }
    }

    /// Returns ones, so the output is the cross-fade weights' quotient (1 wherever any weight is over 1e-4).
    struct Ones;

    impl Restorer for Ones {
        fn restore_segment(&mut self, x16: &[f32]) -> Result<Vec<f32>, String> {
            Ok(vec![1.0; x16.len() * 3])
        }
        fn available(&self) -> bool {
            true
        }
    }

    fn tone(sr: u32, seconds: f64) -> Vec<f32> {
        (0..(sr as f64 * seconds) as usize).map(|i| (i as f32 * 0.05).sin() * 0.3).collect()
    }

    #[test]
    fn linspace_matches_numpy_end_points() {
        assert_eq!(linspace(0.0, 1.0, 5), vec![0.0, 0.25, 0.5, 0.75, 1.0]);
        assert_eq!(linspace(1.0, 0.0, 3), vec![1.0, 0.5, 0.0]);
    }

    #[test]
    fn a_short_clip_is_one_segment_without_progress() {
        let never = AtomicBool::new(false);
        // 21 s at 16 kHz is exactly step + ov: still one segment
        let a = tone(16000, 21.0);
        let mut r = Triple(vec![], true);
        let mut seen = vec![];
        let y = restore(&a, 16000, &mut r, &mut |p| seen.push(p), &never).unwrap();
        assert_eq!((r.0.clone(), y.len(), seen.len()), (vec![336000], 3 * 336000, 0));
        // the input was peak-normalised to 0.9
        let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.9).abs() < 1e-6, "{peak}");
        // 24 kHz in: resampled to 16 kHz first
        let mut r = Triple(vec![], true);
        restore(&tone(24000, 3.0), 24000, &mut r, &mut |_| {}, &never).unwrap();
        assert_eq!(r.0, vec![48000]);
    }

    #[test]
    fn long_audio_runs_in_overlapping_windows_with_progress() {
        let never = AtomicBool::new(false);
        let n = 336001; // one sample over the single-segment limit
        let a = tone(16000, n as f64 / 16000.0);
        assert_eq!(a.len(), n);
        let mut r = Triple(vec![], true);
        let mut seen = vec![];
        let y = restore(&a, 16000, &mut r, &mut |p| seen.push(p), &never).unwrap();
        // starts 0 and 320000: [0, 336000) and [304000, 336001)
        assert_eq!((r.0.clone(), y.len(), seen), (vec![336000, 32001], 3 * n, vec![0.5, 1.0]));
        // repeated samples cross-fade back into the same signal: y is the tripled, normalised input
        let x = audio::highpass(&a, 16000, 50.0).unwrap();
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let worst = (0..3 * n).map(|i| (y[i] - x[i / 3] / peak * 0.9).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "{worst}");
    }

    #[test]
    fn the_cross_fade_weights_sum_to_one_in_the_overlap() {
        let never = AtomicBool::new(false);
        let n = 16000 * 45; // starts 0, 20 s, 40 s
        let a = tone(16000, 45.0);
        let y = restore(&a, 16000, &mut Ones, &mut |_| {}, &never).unwrap();
        assert_eq!(y.len(), 3 * n);
        // ones over weights: 1 everywhere except where both ramps are 0 (the first sample of an up-ramp
        // meets the last of a down-ramp only at their ends), where the sum divides 0 by 1e-4
        let off: Vec<usize> = y.iter().enumerate().filter(|(_, v)| (**v - 1.0).abs() > 1e-5).map(|(i, _)| i).collect();
        assert!(off.is_empty(), "{:?}", &off[..off.len().min(5)]);
    }

    #[test]
    fn the_cancel_flag_stops_before_a_window() {
        let stop = AtomicBool::new(true);
        let mut r = Triple(vec![], true);
        assert!(matches!(restore(&tone(16000, 45.0), 16000, &mut r, &mut |_| {}, &stop), Err(RenderError::Cancelled)));
        assert!(r.0.is_empty());
    }
}
