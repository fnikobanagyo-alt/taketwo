//! The pitch measures and voice-settings mapping of backend/app/delivery.py (step S11a): `prosody` with a hand
//! port of librosa 0.11.0 `yin`, `expressiveness`, `DEFAULT_VOICE_SETTINGS` (one definition:
//! `pipeline::default_voice_settings`), `resolve` and `pick_variant`. `ltas`, `mic_match` and `backfill` are later steps.
//! Parity vectors: rust/golden/record_checks.py -> rust/golden/checks.json, tests/delivery_golden.rs.

use crate::audio::{self, np};
use crate::checks::py_round;
use crate::library::Doc;
use crate::text::py;
use serde_json::Value;

pub use crate::pipeline::default_voice_settings;

/// `prosody`'s dict.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Prosody {
    pub pitch_st: f64,
    pub pitch_range: f64,
    pub dyn_db: f64,
    pub pitch_hz: f64,
}

/// librosa `util.tiny(float32)`: the smallest normal float32.
const TINY32: f64 = f32::MIN_POSITIVE as f64;

/// `librosa.yin(y, fmin=fmin, fmax=fmax, sr=sr, frame_length=frame_length, hop_length=hop)` with librosa 0.11.0's
/// defaults (centred frames, zero padding, trough threshold 0.1). Python runs the difference function in float32
/// (scipy FFT autocorrelation); this sums in float64, then follows Python's float32 steps from the yin values on.
pub fn yin(y: &[f32], fmin: f64, fmax: f64, sr: f64, frame_length: usize, hop: usize) -> Vec<f64> {
    let half = frame_length / 2;
    let mut padded = vec![0.0f32; half];
    padded.extend_from_slice(y);
    padded.extend(std::iter::repeat_n(0.0f32, half));
    assert!(padded.len() >= frame_length, "Input is too short for frame_length");
    let n_frames = 1 + (padded.len() - frame_length) / hop;
    let min_period = (sr / fmax).floor() as usize;
    let max_period = ((sr / fmin).ceil() as usize).min(frame_length - 1);
    let mut out = Vec::with_capacity(n_frames);
    let mut d = vec![0.0f64; max_period + 1];
    for f in 0..n_frames {
        let fr: Vec<f64> = padded[f * hop..f * hop + frame_length].iter().map(|&x| x as f64).collect();
        let acf = |k: usize| -> f64 { fr[..frame_length - k].iter().zip(&fr[k..]).map(|(a, b)| a * b).sum() };
        let acf0 = acf(0);
        // d(k) = 2 * (acf(0) - acf(k)) - cumsum(y^2)[k - 1]; librosa zeroes row 0 first, so k = 1 subtracts 0
        let mut energy = 0.0;
        d[0] = 0.0;
        for k in 1..=max_period {
            let prev = if k == 1 { 0.0 } else { energy };
            d[k] = 2.0 * (acf0 - acf(k)) - prev;
            energy = if k == 1 { fr[0] * fr[0] + fr[1] * fr[1] } else { energy + fr[k] * fr[k] };
        }
        // cumulative mean normalised difference over min_period..=max_period, stored as float32 as librosa does
        let mut cum = 0.0;
        let mut cm = vec![0.0f64; max_period];
        for k in 1..=max_period {
            cum += d[k];
            cm[k - 1] = cum / k as f64;
        }
        let yv: Vec<f32> = (min_period..=max_period).map(|k| (d[k] / (cm[k - 1] + TINY32)) as f32).collect();
        let n = yv.len();
        // parabolic shifts (numba stencil in float64, written to float32; 0 at both edges)
        let shift = |i: usize| -> f32 {
            if i == 0 || i + 1 >= n {
                return 0.0;
            }
            let (l, c, r) = (yv[i - 1] as f64, yv[i] as f64, yv[i + 1] as f64);
            let a = r + l - 2.0 * c;
            let b = (r - l) / 2.0;
            if b.abs() >= a.abs() { 0.0 } else { (-b / a) as f32 }
        };
        let trough = |i: usize| -> bool {
            if i == 0 {
                yv[0] < yv[1]
            } else if i + 1 == n {
                yv[i] < yv[i - 1]
            } else {
                yv[i] < yv[i - 1] && yv[i] <= yv[i + 1]
            }
        };
        let thr = 0.1f32;
        let period = match (0..n).find(|&i| trough(i) && yv[i] < thr) {
            Some(i) => i,
            None => (0..n).fold(0, |best, i| if yv[i] < yv[best] { i } else { best }),
        };
        out.push(sr / ((min_period + period) as f64 + shift(period) as f64));
    }
    out
}

/// `np.median` of float64 values.
fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { np::mean_f64(&s[n / 2 - 1..=n / 2]) }
}

/// `np.std` of float64 values (pairwise sums, as numpy).
fn std_f64(v: &[f64]) -> f64 {
    let m = np::mean_f64(v);
    let sq: Vec<f64> = v.iter().map(|x| (x - m) * (x - m)).collect();
    (np::sum_f64(&sq) / v.len() as f64).sqrt()
}

/// `np.std` of float32 values: numpy keeps the mean, the squares, the sum and the root in float32.
fn std_f32(v: &[f32]) -> f32 {
    let m = (np::sum_f32(v) as f64 / v.len() as f64) as f32;
    let sq: Vec<f32> = v.iter().map(|x| (x - m) * (x - m)).collect();
    ((np::sum_f32(&sq) as f64 / v.len() as f64) as f32).sqrt()
}

/// `prosody(a, sr)`: pitch movement (semitone std and 5-95 % range), median pitch and loudness spread over voiced frames.
pub fn prosody(a: &[f32], sr: u32) -> Prosody {
    let x = audio::resample(a, sr, 16000);
    if (x.len() as f64) < 16000.0 * 0.8 {
        return Prosody::default();
    }
    let f0 = yin(&x, 60.0, 420.0, 16000.0, 1024, 320);
    let db = audio::frame_db(&x, 16000, audio::FRAME_MS);
    let n = f0.len().min(db.len());
    let (f0, db) = (&f0[..n], &db[..n]);
    let line = np::percentile_f32(db, 90.0).expect("frames") - 20.0f32;
    let voiced: Vec<usize> = (0..n).filter(|&i| db[i] > line && f0[i] > 62.0 && f0[i] < 410.0).collect();
    if voiced.len() < 10 {
        return Prosody::default();
    }
    let fv: Vec<f64> = voiced.iter().map(|&i| f0[i]).collect();
    let med = median(&fv);
    let st: Vec<f64> = fv.iter().map(|f| 12.0 * (f / med).log2()).filter(|s| s.abs() < 12.0).collect(); // drop octave errors
    if st.len() < 10 {
        return Prosody::default();
    }
    let dv: Vec<f32> = voiced.iter().map(|&i| db[i]).collect();
    Prosody {
        pitch_st: std_f64(&st),
        pitch_range: np::percentile_f64(&st, 95.0).unwrap() - np::percentile_f64(&st, 5.0).unwrap(),
        dyn_db: std_f32(&dv) as f64,
        pitch_hz: med,
    }
}

/// `expressiveness(p)`: one raw score, only meaningful against the same speaker.
pub fn expressiveness(p: &Prosody) -> f64 {
    p.pitch_st + 0.35 * p.pitch_range / 4.0 + 0.25 * p.dyn_db / 3.0
}

/// `resolve(vs, spec, base_params, qc)`: friendly voice settings onto engine params (`keys` = the spec's
/// parameter keys) and QC thresholds. Explicit advanced params win.
pub fn resolve(vs: Option<&Doc>, keys: &[&str], base_params: Option<&Doc>, qc: &Doc) -> Result<(Doc, Doc), String> {
    let mut v = default_voice_settings();
    if let Some(vs) = vs {
        v.extend(vs.clone());
    }
    let (stability, similarity, style) = (py::num(&v["stability"])?, py::num(&v["similarity"])?, py::num(&v["style"])?);
    let has = |k: &str| keys.contains(&k);
    let mut params = Doc::new();
    if has("temperature") {
        let t = 1.05 - 0.55 * stability + 0.2 * style;
        let t = if t > 0.35 { t } else { 0.35 };
        params.insert("temperature".into(), py_round(if 1.15 > t { t } else { 1.15 }, 3).into());
    }
    if has("cfg_value") {
        params.insert("cfg_value".into(), py_round(1.4 + 1.6 * similarity, 2).into());
    }
    if has("cfg_weight") {
        params.insert("cfg_weight".into(), py_round(0.3 + 0.4 * similarity - 0.15 * style, 2).into());
    }
    if has("exaggeration") {
        params.insert("exaggeration".into(), py_round(0.4 + 0.8 * style, 2).into());
    }
    if has("guidance_scale") {
        params.insert("guidance_scale".into(), py_round(1.5 + 1.5 * similarity, 2).into());
    }
    for (k, val) in base_params.into_iter().flatten() {
        if !val.is_null() {
            params.insert(k.clone(), val.clone());
        }
    }
    let mut qc = qc.clone();
    qc.insert("min_speaker_similarity".into(), py_round(0.70 + 0.2 * similarity, 3).into());
    Ok((params, qc))
}

/// `pick_variant(profile, style)`: style 0 keeps the chosen clip; higher styles move to more expressive clips.
/// Returns the chosen variant's `index` (or `active_variant`) as stored.
pub fn pick_variant(profile: &Doc, style: f64) -> Result<Value, String> {
    let active = profile.get("active_variant").cloned().unwrap_or(Value::from(0));
    let empty = Vec::new();
    let vs = match profile.get("variants") {
        Some(v) if py::truthy(v) => v.as_array().ok_or("variants is not a list")?,
        _ => &empty,
    };
    if style < 0.2 || vs.is_empty() || vs[0].get("expressiveness").is_none() {
        return Ok(active);
    }
    let f = |v: &Value, k: &str| -> Result<f64, String> { py::num(v.get(k).ok_or_else(|| format!("KeyError: '{k}'"))?) };
    let mut best_q = f(&vs[0], "score")?;
    for v in vs {
        let s = f(v, "score")?;
        if s > best_q {
            best_q = s;
        }
    }
    let mut usable = Vec::new();
    for v in vs {
        if f(v, "score")? >= best_q - 0.25 {
            usable.push(v);
        }
    }
    // max() and min() with a key keep the first of equal values
    let mut target = usable[0];
    for v in &usable {
        if f(v, "expressiveness")? > f(target, "expressiveness")? {
            target = v;
        }
    }
    if style < 0.5 {
        let cur = &vs[py::int(&active)? as usize];
        let cur_e = match cur.get("expressiveness") {
            Some(e) => py::num(e)?,
            None => 0.0,
        };
        let mut mid = Vec::new();
        for v in &usable {
            if f(v, "expressiveness")? >= cur_e {
                mid.push(*v);
            }
        }
        if !mid.is_empty() {
            let dist = |v: &Value| -> Result<f64, String> { Ok((f(v, "expressiveness")? - (cur_e + 1.0) / 2.0).abs()) };
            target = mid[0];
            for v in &mid {
                if dist(v)? < dist(target)? {
                    target = v;
                }
            }
        }
    }
    target.get("index").cloned().ok_or_else(|| "KeyError: 'index'".into())
}
