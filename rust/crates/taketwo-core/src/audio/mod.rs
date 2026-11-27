//! Port of backend/app/audio.py (step S10): WAV I/O, resampling, filters, loudness, silence analysis,
//! assembly helpers and the sentence-end analysis. Names, arguments and defaults follow Python (Rust has
//! no default arguments: each default is in the doc comment). Precision follows numpy 2 step by step:
//! float32 where numpy computes in float32 (frame levels, percentiles, thresholds, gains), float64 where
//! Python uses Python floats (frame seconds, `int(seconds * rate)`, returned seconds).
//! Parity vectors: rust/golden/record_audio.py -> rust/golden/audio.json, tests/audio_golden.rs.

pub mod ffmpeg;
pub mod loudness;
pub mod noisereduce;
pub mod np;
pub mod signal;
pub mod wav;

pub use wav::{load, save};

use np::{mean_f32, percentile_f32, py_slice};

/// A Python exception a ported function would raise: its type name and message.
#[derive(Debug, Clone, PartialEq)]
pub struct PyError {
    pub kind: &'static str,
    pub message: String,
}

impl PyError {
    pub fn value(m: &str) -> Self {
        PyError { kind: "ValueError", message: m.into() }
    }
    pub fn index(m: &str) -> Self {
        PyError { kind: "IndexError", message: m.into() }
    }
}

/// `int(sr * ms / 1000)`: Python's true division, then truncation.
fn samples(sr: u32, ms: f64) -> usize {
    (sr as f64 * ms / 1000.0) as usize
}

/// `audio.resample(a, sr_from, sr_to)`.
pub fn resample(a: &[f32], sr_from: u32, sr_to: u32) -> Vec<f32> {
    if sr_from == sr_to {
        return a.to_vec();
    }
    signal::resample_poly(a, sr_to as u64, sr_from as u64)
}

/// `audio.highpass(a, sr, hz)` (Python default hz = 60.0): 4th-order Butterworth, forward and backward.
pub fn highpass(a: &[f32], sr: u32, hz: f64) -> Result<Vec<f32>, PyError> {
    let sos = signal::butter(4, &[hz], signal::BType::Highpass, sr as f64)?;
    Ok(signal::sosfiltfilt(&sos, a)?.into_iter().map(|v| v as f32).collect())
}

/// `audio.loudness(a, sr)`: integrated LUFS (pyloudnorm), -inf under 0.5 s.
pub fn loudness(a: &[f32], sr: u32) -> f64 {
    if a.len() < (0.5 * sr as f64) as usize {
        return f64::NEG_INFINITY;
    }
    loudness::integrated_loudness(a, sr)
}

/// `audio.normalize_lufs(a, sr, target, peak_db)` (Python default peak_db = -1.0).
pub fn normalize_lufs(a: &[f32], sr: u32, target: f64, peak_db: f64) -> Vec<f32> {
    normalize_lufs_with(a, loudness(a, sr), target, peak_db)
}

/// normalize_lufs after its loudness measurement `cur` (split out so a test can feed Python's value).
pub fn normalize_lufs_with(a: &[f32], cur: f64, target: f64, peak_db: f64) -> Vec<f32> {
    if !cur.is_finite() {
        return a.to_vec();
    }
    let gain = 10f64.powf((target - cur) / 20.0) as f32;
    let out: Vec<f32> = a.iter().map(|x| x * gain).collect();
    let peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let lim = 10f64.powf(peak_db / 20.0);
    if peak > lim as f32 { soft_limit(&out, lim) } else { out }
}

/// `audio.soft_limit(a, ceiling)`: tanh knee above 80 % of the ceiling.
pub fn soft_limit(a: &[f32], ceiling: f64) -> Vec<f32> {
    let knee = 0.8 * ceiling;
    let (k32, span) = (knee as f32, (ceiling - knee) as f32);
    a.iter()
        .map(|&x| {
            if x.abs() > k32 {
                let over = x.abs() - k32;
                let s = if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 };
                s * (k32 + span * (over / span).tanh())
            } else {
                x
            }
        })
        .collect()
}

// ---------------------------------------------------------------- analysis

pub const FRAME_MS: u32 = 20;

/// `audio.frame_db(a, sr, frame_ms)` (Python default frame_ms = FRAME_MS = 20): float32 frame levels.
pub fn frame_db(a: &[f32], sr: u32, frame_ms: u32) -> Vec<f32> {
    let n = samples(sr, frame_ms as f64);
    if a.len() < n || n == 0 {
        return vec![-120.0];
    }
    a[..a.len() / n * n]
        .chunks_exact(n)
        .map(|f| {
            let sq: Vec<f32> = f.iter().map(|x| x * x).collect();
            let rms = (mean_f32(&sq) + 1e-12f32).sqrt();
            20.0f32 * (rms + 1e-12f32).log10()
        })
        .collect()
}

/// `audio.speech_threshold_db(db)`: halfway between noise floor and speech level, clamped.
pub fn speech_threshold_db(db: &[f32]) -> Result<f64, PyError> {
    let floor = percentile_f32(db, 10.0)?;
    let speech = percentile_f32(db, 90.0)?;
    let mix = floor + 0.35f32 * (speech - floor);
    let s18 = speech - 18.0f32;
    let lo = if s18 < mix { s18 } else { mix };
    Ok(if -60.0 > lo { -60.0 } else { lo as f64 })
}

#[derive(Debug, Clone, PartialEq)]
pub struct SilenceReport {
    pub lead: f64,
    pub tail: f64,
    pub max_internal: f64,
    /// (start, end) seconds
    pub internal_gaps: Vec<(f64, f64)>,
    pub speech_ratio: f64,
}

/// `audio.silence_report(a, sr, min_gap)` (Python default min_gap = 0.25).
pub fn silence_report(a: &[f32], sr: u32, min_gap: f64) -> SilenceReport {
    let db = frame_db(a, sr, FRAME_MS);
    let thr = speech_threshold_db(&db).expect("frame_db is never empty") as f32;
    let voiced: Vec<bool> = db.iter().map(|&d| d > thr).collect();
    let fs = FRAME_MS as f64 / 1000.0;
    let idx: Vec<usize> = (0..voiced.len()).filter(|&i| voiced[i]).collect();
    if idx.is_empty() {
        let d = a.len() as f64 / sr as f64;
        return SilenceReport { lead: d, tail: d, max_internal: d, internal_gaps: vec![], speech_ratio: 0.0 };
    }
    let (first, last) = (idx[0], idx[idx.len() - 1]);
    let mut gaps = Vec::new();
    let mut run_start: Option<usize> = None;
    for i in first..=last {
        if !voiced[i] {
            run_start.get_or_insert(i);
        } else if let Some(rs) = run_start.take() {
            let dur = (i - rs) as f64 * fs;
            if dur >= min_gap {
                gaps.push((rs as f64 * fs, i as f64 * fs));
            }
        }
    }
    let max_internal = gaps.iter().map(|(s, e)| e - s).fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| if d > m { d } else { m })));
    SilenceReport {
        lead: first as f64 * fs,
        tail: (voiced.len() - 1 - last) as f64 * fs,
        max_internal: max_internal.unwrap_or(0.0),
        internal_gaps: gaps,
        speech_ratio: idx.len() as f64 / voiced.len() as f64,
    }
}

/// Python's `_floor_threshold(db)`: 48 dB under the speech level (float32, returned as a Python float).
pub fn floor_threshold(db: &[f32]) -> Result<f64, PyError> {
    Ok((percentile_f32(db, 90.0)? - 48.0f32) as f64)
}

/// `audio.trim_edges(a, sr, keep_ms, tail_ms)` (Python defaults 60 and 220). Returns the slice of `a`, as Python does.
pub fn trim_edges(a: &[f32], sr: u32, keep_ms: u32, tail_ms: u32) -> &[f32] {
    let db = frame_db(a, sr, FRAME_MS);
    if db.len() < 3 {
        return a;
    }
    let thr = floor_threshold(&db).expect("non-empty") as f32;
    let idx: Vec<usize> = (0..db.len()).filter(|&i| db[i] > thr).collect();
    if idx.is_empty() {
        return a;
    }
    let fs = FRAME_MS as f64 / 1000.0;
    let start = (idx[0] as f64 * fs - keep_ms as f64 / 1000.0).max(0.0);
    let end = ((idx[idx.len() - 1] + 1) as f64 * fs + tail_ms as f64 / 1000.0).min(a.len() as f64 / sr as f64);
    &a[py_slice(a.len(), (start * sr as f64) as i64, (end * sr as f64) as i64)]
}

/// `audio.speech_start(a, sr, keep_ms)` (Python default keep_ms = 100).
pub fn speech_start(a: &[f32], sr: u32, keep_ms: u32) -> f64 {
    let db = frame_db(a, sr, 10);
    let thr = floor_threshold(&db).expect("non-empty") as f32;
    match db.iter().position(|&d| d > thr) {
        Some(i) => (i as f64 * 0.01 - keep_ms as f64 / 1000.0).max(0.0),
        None => 0.0,
    }
}

/// `audio.ending_level(a, sr)`: level (dB relative to speech) of the last non-silent 20 ms frame.
pub fn ending_level(a: &[f32], sr: u32) -> f64 {
    let db = frame_db(a, sr, FRAME_MS);
    if db.len() < 5 {
        return -99.0;
    }
    let speech = percentile_f32(&db, 90.0).expect("non-empty");
    match db.iter().rposition(|&d| d > speech - 60.0f32) {
        Some(i) => (db[i] - speech) as f64,
        None => -99.0,
    }
}

/// `audio.shorten_gaps(a, sr, gaps, max_gap)`: collapse internal silences longer than max_gap (with fades).
pub fn shorten_gaps(a: &[f32], sr: u32, gaps: &[(f64, f64)], max_gap: f64) -> Vec<f32> {
    if gaps.is_empty() {
        return a.to_vec();
    }
    let mut out = Vec::with_capacity(a.len());
    let mut cursor: i64 = 0;
    for &(s, e) in gaps {
        if e - s <= max_gap {
            continue;
        }
        let keep_half = max_gap / 2.0;
        let cut_a = ((s + keep_half) * sr as f64) as i64;
        let cut_b = ((e - keep_half) * sr as f64) as i64;
        out.extend(fade(&a[py_slice(a.len(), cursor, cut_a)], sr, 0, 8));
        cursor = cut_b;
    }
    out.extend(fade(&a[py_slice(a.len(), cursor, a.len() as i64)], sr, 8, 0));
    out
}

/// `audio.fade(a, sr, fade_in_ms, fade_out_ms)` (Python defaults 0): squared linear ramps.
pub fn fade(a: &[f32], sr: u32, fade_in_ms: u32, fade_out_ms: u32) -> Vec<f32> {
    let mut a = a.to_vec();
    if fade_in_ms != 0 {
        let n = a.len().min(samples(sr, fade_in_ms as f64));
        for (x, w) in a[..n].iter_mut().zip(np::linspace_f32(0.0, 1.0, n)) {
            *x *= w * w;
        }
    }
    if fade_out_ms != 0 {
        let n = a.len().min(samples(sr, fade_out_ms as f64));
        let len = a.len();
        for (x, w) in a[len - n..].iter_mut().zip(np::linspace_f32(1.0, 0.0, n)) {
            *x *= w * w;
        }
    }
    a
}

/// `audio.clipping_ratio(a)`: share of samples above 0.985 (compared in float32).
pub fn clipping_ratio(a: &[f32]) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    a.iter().filter(|x| x.abs() > 0.985f32).count() as f64 / a.len() as f64
}

/// `audio.noise_bursts(a, sr)`: longest run (seconds) of loud, noise-like 30 ms frames.
pub fn noise_bursts(a: &[f32], sr: u32) -> f64 {
    let n = (0.03 * sr as f64) as usize;
    if a.len() < n * 4 || n == 0 {
        return 0.0;
    }
    let win = np::hanning(n);
    let fft = realfft::RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let mut flat = Vec::new();
    let mut db = Vec::new();
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    for f in a[..a.len() / n * n].chunks_exact(n) {
        for ((b, &x), w) in buf.iter_mut().zip(f).zip(&win) {
            *b = x as f64 * w;
        }
        fft.process(&mut buf, &mut spec).expect("fft sizes");
        let p: Vec<f64> = spec.iter().map(|c| c.re.hypot(c.im).powi(2) + 1e-12).collect();
        let logs: Vec<f64> = p.iter().map(|v| v.ln()).collect();
        let mean = np::mean_f64(&p);
        flat.push(np::mean_f64(&logs).exp() / mean);
        db.push(10.0 * mean.log10());
    }
    let line = np::percentile_f64(&db, 90.0).expect("non-empty") - 25.0;
    let (mut best, mut run) = (0usize, 0usize);
    for (d, fl) in db.iter().zip(&flat) {
        run = if *d > line && *fl > 0.1 { run + 1 } else { 0 };
        best = best.max(run);
    }
    (best * n) as f64 / sr as f64
}

/// `audio.estimate_snr(a, sr)`: 95th minus 10th percentile of the frame levels.
pub fn estimate_snr(a: &[f32], sr: u32) -> f64 {
    let db = frame_db(a, sr, FRAME_MS);
    (percentile_f32(&db, 95.0).expect("non-empty") - percentile_f32(&db, 10.0).expect("non-empty")) as f64
}

/// `audio.concat_with_pauses(chunks, pauses_ms, sr)`: joins with click fades only on silent edges.
/// `pauses_ms` needs one entry per gap (Python raises IndexError otherwise; this panics).
pub fn concat_with_pauses(chunks: &[&[f32]], pauses_ms: &[i64], sr: u32) -> Vec<f32> {
    let mut out = Vec::new();
    let quiet = |s: &[f32]| s.iter().fold(0.0f32, |m, x| m.max(x.abs())) < 1e-6f32;
    for (i, c) in chunks.iter().enumerate() {
        let head = samples(sr, 6.0);
        let tail = samples(sr, 25.0);
        let fi = if c.len() >= head && quiet(&c[..head]) { 6 } else { 0 };
        let fo = if c.len() >= tail && quiet(&c[c.len() - tail..]) { 25 } else { 0 };
        out.extend(fade(c, sr, fi, fo));
        if i + 1 < chunks.len() {
            out.extend(std::iter::repeat_n(0.0f32, samples(sr, pauses_ms[i] as f64)));
        }
    }
    out
}

// ---------------------------------------------------------------- sentence-end analysis

/// `audio.sound_regions(a, sr, bridge_ms)` (Python default bridge_ms = 130): regions of sound in 10 ms
/// frames (half-open frame ranges), the levels, and the speech level.
pub fn sound_regions(a: &[f32], sr: u32, bridge_ms: u32) -> (Vec<(usize, usize)>, Vec<f32>, f64) {
    let db = frame_db(a, sr, 10);
    let n = db.len();
    let sp32 = percentile_f32(&db, 90.0).expect("non-empty");
    let sp = sp32 as f64;
    let low = percentile_f32(&db, 10.0).expect("non-empty") + 10.0f32;
    let alt = (sp - 20.0) as f32; // min() compares the float32 with the Python float in float32
    let thr = if alt < low { alt } else { low };
    let voiced: Vec<bool> = db.iter().map(|&d| d >= thr).collect();
    let gap = ((bridge_ms as f64 / 10.0) as usize).max(1);
    let mut regions = Vec::new();
    let mut i = 0;
    while i < n {
        if !voiced[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n {
            if voiced[j] {
                j += 1;
                continue;
            }
            let mut k = j;
            while k < n && !voiced[k] {
                k += 1;
            }
            if k < n && (k - j) < gap {
                j = k;
            } else {
                break;
            }
        }
        regions.push((i, j));
        i = j;
    }
    (regions, db, sp)
}

/// The dict `audio.end_analysis` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct EndAnalysis {
    pub start: f64,
    pub cut: f64,
    pub natural: bool,
    pub junk: bool,
    pub last_level: f64,
}

/// `audio.end_analysis(a, sr, last_word_end, tail_s)` (Python default tail_s = 0.12).
pub fn end_analysis(a: &[f32], sr: u32, last_word_end: f64, tail_s: f64) -> EndAnalysis {
    let (regions, db, sp) = sound_regions(a, sr, 130);
    let n = db.len();
    let dur = a.len() as f64 / sr as f64;
    if regions.is_empty() {
        return EndAnalysis { start: 0.0, cut: n as f64 * 0.01, natural: true, junk: false, last_level: -99.0 };
    }
    let lw = ((last_word_end - 0.05).max(0.0) / 0.01) as i64;
    let idx = regions.iter().position(|&(_, e0)| e0 as i64 >= lw).unwrap_or(regions.len() - 1);
    let (s0, e0) = regions[idx];
    let mut end = terminal_end(&db, (s0 as i64).max(lw - 15), sp).unwrap_or(e0);
    let sp32 = sp as f32;
    let junk = regions[idx + 1..].iter().any(|&(s1, e1)| {
        e1 - s1 >= 8 && s1 >= end && ((percentile_f32(&db[s1..e1], 90.0).expect("non-empty") - sp32) as f64) > -27.0
    });
    let mut judge = end;
    if !junk {
        let thr = (sp - 48.0) as f32;
        if let Some(last) = db.iter().rposition(|&d| d > thr)
            && last + 1 > end
        {
            let tail_max = db[end..last + 1].iter().fold(f32::NEG_INFINITY, |m, &v| m.max(v));
            if tail_max as f64 >= sp - 30.0 {
                judge = last + 1;
            }
            end = last + 1;
        }
    }
    let tail_frames = ((tail_s / 0.01) as usize).min(n.saturating_sub(end));
    let cut = if end + tail_frames >= n { dur } else { (end + tail_frames) as f64 * 0.01 };
    let levels: Vec<f32> = db[s0.max(judge.saturating_sub(2)).min(judge)..judge]
        .iter()
        .map(|&d| 10f32.powf((d - sp32) / 10.0f32))
        .collect();
    let last_level = (10.0f32 * mean_f32(&levels).log10()) as f64;
    EndAnalysis {
        start: regions[0].0 as f64 * 0.01,
        cut: if cut < dur { cut } else { dur },
        natural: last_level <= -24.0,
        junk,
        last_level,
    }
}

/// 250 ms of silence closes a word; the next phrase starts after it.
pub const TERMINAL_SILENCE_FRAMES: usize = 25;

/// Python's `_terminal_end(db, scan0, sp)`: frame just after the last audible frame that follows the last word.
pub fn terminal_end(db: &[f32], scan0: i64, sp: f64) -> Option<usize> {
    let thr = (sp - 48.0) as f32;
    let (mut end, mut quiet) = (None, 0);
    for (i, &d) in db.iter().enumerate().skip(scan0.max(0) as usize) {
        if d > thr {
            end = Some(i + 1);
            quiet = 0;
        } else {
            quiet += 1;
            if quiet >= TERMINAL_SILENCE_FRAMES {
                break;
            }
        }
    }
    end
}

/// `audio.lead_burst(a, sr)`: (start, length) seconds of a short hiss opening the take, set apart from what follows.
pub fn lead_burst(a: &[f32], sr: u32) -> Option<(f64, f64)> {
    let n = (0.01 * sr as f64) as usize;
    if n == 0 {
        return None;
    }
    let m = a.len().min((1.2 * sr as f64) as usize) / n;
    if m < 20 {
        return None;
    }
    let full: Vec<f32> = a[..m * n]
        .chunks_exact(n)
        .map(|f| {
            let sq: Vec<f32> = f.iter().map(|x| x * x).collect();
            20.0f32 * (mean_f32(&sq).sqrt() + 1e-9f32).log10()
        })
        .collect();
    let sos = signal::butter(4, &[4000.0, (sr as f64 / 2.0 * 0.95).min(10000.0)], signal::BType::Bandpass, sr as f64).ok()?;
    let band = signal::sosfiltfilt(&sos, a).ok()?; // shortcut: Python raises here for a < 28 samples, unreachable at real rates
    let high: Vec<f64> = band[..m * n]
        .chunks_exact(n)
        .map(|f| {
            let sq: Vec<f64> = f.iter().map(|x| x * x).collect();
            20.0 * (np::mean_f64(&sq).sqrt() + 1e-9).log10()
        })
        .collect();
    let line = percentile_f32(&frame_db(a, sr, 10), 90.0).expect("non-empty") - 30.0f32;
    let loud: Vec<bool> = full.iter().map(|&f| f > line).collect();
    let start = loud.iter().position(|&l| l)?;
    let end = loud[start..].iter().position(|&l| !l).map_or(m, |k| start + k);
    let rest = loud[end..].iter().position(|&l| l);
    let diff: Vec<f64> = (start..end).map(|i| high[i] - full[i] as f64).collect();
    let len = end - start;
    if rest.is_none() || !(4..=30).contains(&len) || rest.unwrap() < 15 || np::mean_f64(&diff) <= -5.0 {
        return None;
    }
    Some((start as f64 * 0.01, len as f64 * 0.01))
}

/// `audio.edge_silence(a, sr)`: (lead, tail) seconds of real silence at the edges of a part.
pub fn edge_silence(a: &[f32], sr: u32) -> (f64, f64) {
    let (regions, db, _) = sound_regions(a, sr, 60);
    match (regions.first(), regions.last()) {
        (Some(f), Some(l)) => (f.0 as f64 * 0.01, (db.len() - l.1) as f64 * 0.01),
        _ => (0.0, 0.0),
    }
}
