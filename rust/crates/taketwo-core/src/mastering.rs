//! The mastering stages of backend/app/studio.py (step S11b): `expander`, `de_ess`, `master_chain`.
//! Precision follows numpy: frame levels in float32, the envelopes and gains in float64, `astype(float32)` at the end.
//! Where Python raises on a short clip (scipy's padlen check, or numpy's broadcast error when `np.convolve`'s
//! "same" output takes the kernel's length), these return the same exception as a `PyError`.
//! Parity vectors: rust/golden/record_mastering.py -> rust/golden/mastering.json, tests/mastering_golden.rs.

use crate::audio::{self, PyError, np, signal};

/// numpy's broadcast check for two 1-D arrays (lengths 1 broadcast; no 1-length case reaches here).
fn same_shape(a: usize, b: usize) -> Result<(), PyError> {
    if a == b {
        Ok(())
    } else {
        Err(PyError::value(&format!("operands could not be broadcast together with shapes ({a},) ({b},) ")))
    }
}

/// `np.ones(n) / n`.
fn box_kernel(n: usize) -> Vec<f64> {
    vec![1.0 / n as f64; n]
}

/// The expander's 5 ms frames: hop, float32 levels and their 90th percentile; None under 20 frames.
fn expander_levels(a: &[f32], sr: u32) -> Option<(usize, Vec<f32>, f32)> {
    let hop = ((0.005 * sr as f64) as usize).max(1);
    let n = a.len() / hop;
    if n < 20 {
        return None;
    }
    let lvl: Vec<f32> = a[..n * hop]
        .chunks_exact(hop)
        .map(|f| {
            let sq: Vec<f32> = f.iter().map(|x| x * x).collect();
            10.0f32 * (np::mean_f32(&sq) + 1e-12f32).log10()
        })
        .collect();
    let p90 = np::percentile_f32(&lvl, 90.0).expect("20 frames or more");
    Some((hop, lvl, p90))
}

fn protected_mask(lvl: &[f32], p90: f32, guard_ms: u32) -> Vec<bool> {
    let guard = ((guard_ms as f64 / 5.0) as usize).max(1);
    // np.convolve(np.pad(near_speech, guard), np.ones(2 * guard + 1), "valid") > 0: any near-speech frame within guard
    let near: Vec<bool> = lvl.iter().map(|&l| l > p90 - 42.0f32).collect();
    (0..lvl.len())
        .map(|i| near[i.saturating_sub(guard)..(i + guard + 1).min(near.len())].iter().any(|&b| b))
        .collect()
}

/// The expander's protected frames (speech and anything within `guard_ms` of it), None when the clip has
/// fewer than 20 frames (the expander then returns its input).
pub fn expander_protected(a: &[f32], sr: u32, guard_ms: u32) -> Option<Vec<bool>> {
    expander_levels(a, sr).map(|(_, lvl, p90)| protected_mask(&lvl, p90, guard_ms))
}

/// `expander(a, sr, below_db, max_cut_db, guard_ms)` (Python defaults 45.0, 8.0, 300): lowers unprotected frames
/// more than `below_db` under the speech level by up to `max_cut_db`, with ~10 ms attack and ~150 ms release.
pub fn expander(a: &[f32], sr: u32, below_db: f64, max_cut_db: f64, guard_ms: u32) -> Vec<f32> {
    let Some((hop, lvl, p90)) = expander_levels(a, sr) else {
        return a.to_vec();
    };
    let thr = p90 - below_db as f32;
    let protected = protected_mask(&lvl, p90, guard_ms);
    let target: Vec<f32> = lvl
        .iter()
        .zip(&protected)
        .map(|(&l, &p)| if p { 0.0 } else { -((thr - l) * 1.0f32).clamp(0.0, max_cut_db as f32) })
        .collect();
    let (att, rel) = ((-1.0f64 / 2.0).exp(), (-1.0f64 / 30.0).exp());
    let mut cur = 0.0f64;
    let g: Vec<f64> = target
        .iter()
        .map(|&t| {
            let t = t as f64;
            let coef = if t > cur { att } else { rel };
            cur = coef * cur + (1.0 - coef) * t;
            cur
        })
        .collect();
    let xp: Vec<f64> = (0..g.len()).map(|i| (i * hop) as f64 + hop as f64 / 2.0).collect();
    let x: Vec<f64> = (0..a.len()).map(|i| i as f64).collect();
    let gain = np::interp(&x, &xp, &g);
    a.iter().zip(gain).map(|(&s, gdb)| (s as f64 * 10f64.powf(gdb / 20.0)) as f32).collect()
}

/// `de_ess(y, sr, strength)` (Python default strength = 1.0): pulls the 4.5-9.5 kHz band down (up to 65 %) where it
/// spikes against the whole signal, only in loud speech. Below 20 kHz the input is returned unchanged.
pub fn de_ess(y: &[f32], sr: u32, strength: f64) -> Result<Vec<f32>, PyError> {
    if sr < 20000 {
        return Ok(y.to_vec());
    }
    let sos = signal::butter(4, &[4500.0, (sr as f64 / 2.0 * 0.95).min(9500.0)], signal::BType::Bandpass, sr as f64)?;
    let s = signal::sosfiltfilt(&sos, y)?;
    let n = (0.004 * sr as f64) as usize;
    let k = box_kernel(n);
    let s2: Vec<f64> = s.iter().map(|v| v * v).collect();
    let y2: Vec<f64> = y.iter().map(|v| (v * v) as f64).collect(); // y**2 is squared in float32
    let env_s: Vec<f64> = np::convolve_same(&s2, &k).iter().map(|v| (v + 1e-12).sqrt()).collect();
    let env_a: Vec<f64> = np::convolve_same(&y2, &k).iter().map(|v| (v + 1e-12).sqrt()).collect();
    let reduce: Vec<f64> =
        env_s.iter().zip(&env_a).map(|(es, ea)| ((es / ea - 0.35) * 1.6 * strength).clamp(0.0, 0.65)).collect();
    let loud_db: Vec<f64> = env_a.iter().map(|e| 20.0 * e.log10()).collect();
    let line = np::percentile_f64(&loud_db, 90.0)? - 14.0;
    let loud: Vec<f64> = loud_db.iter().map(|&d| if d > line { 1.0 } else { 0.0 }).collect();
    let k3 = box_kernel(3 * n);
    let loud = np::convolve_same(&loud, &k3);
    same_shape(reduce.len(), loud.len())?;
    let reduce: Vec<f64> = reduce.iter().zip(&loud).map(|(r, l)| r * l).collect();
    let reduce = np::convolve_same(&reduce, &k3);
    same_shape(s.len(), reduce.len())?;
    Ok(y.iter().zip(&s).zip(&reduce).map(|((&yv, sv), r)| (yv as f64 - sv * r) as f32).collect())
}

/// `master_chain(a, sr)`: 75 Hz high-pass, de-ess, 2:1 compression above -24 dBFS (30 ms RMS, smoothed gain), expander.
pub fn master_chain(a: &[f32], sr: u32) -> Result<Vec<f32>, PyError> {
    let y = audio::highpass(a, sr, 75.0)?;
    let y = de_ess(&y, sr, 1.0)?;
    let n = (0.03 * sr as f64) as usize;
    let k = box_kernel(n);
    let y2: Vec<f64> = y.iter().map(|v| (v * v) as f64).collect();
    let g: Vec<f64> = np::convolve_same(&y2, &k)
        .iter()
        .map(|v| {
            let lvl = 20.0 * (v + 1e-12).sqrt().log10();
            let over = (lvl - (-24.0)).max(0.0);
            10f64.powf(-(over * 0.5) / 20.0)
        })
        .collect();
    let g = np::convolve_same(&g, &k);
    same_shape(y.len(), g.len())?;
    let y: Vec<f32> = y.iter().zip(&g).map(|(&v, gv)| (v as f64 * gv) as f32).collect();
    Ok(expander(&y, sr, 45.0, 8.0, 300))
}
