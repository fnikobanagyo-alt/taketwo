//! The pitch measures and voice-settings mapping of backend/app/delivery.py (step S11a): `prosody` with a hand
//! port of librosa 0.11.0 `yin`, `expressiveness`, `DEFAULT_VOICE_SETTINGS` (one definition:
//! `pipeline::default_voice_settings`), `resolve` and `pick_variant`. Step S11b adds the mic match: `ltas`,
//! `mic_match` and the NaN-tolerant `mic.json` reader. `backfill` is a later step.
//! Parity vectors: rust/golden/record_checks.py -> rust/golden/checks.json, tests/delivery_golden.rs;
//! rust/golden/record_mastering.py -> rust/golden/mastering.json, tests/mastering_golden.rs.

use crate::audio::{self, np};
use crate::checks::py_round;
use crate::library::Doc;
use crate::text::py;
use serde_json::Value;
use std::path::Path;

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

// ---------------------------------------------------------------- mic match

/// `BANDS`: 24 third-octave centres, 80 Hz to about 16 kHz (`80 * 2 ** (i / 3)`).
pub fn bands() -> Vec<f64> {
    (0..24).map(|i| 80.0 * 2f64.powf(i as f64 / 3.0)).collect()
}

/// `ltas(a, sr)`: long-term average spectrum of the louder 2048-sample frames in the 24 bands (dB). Empty under
/// 8192 samples; NaN for a band with no FFT bin or at or above 0.92 x Nyquist.
pub fn ltas(a: &[f32], sr: u32) -> Vec<f64> {
    let n = 2048;
    if a.len() < n * 4 {
        return Vec::new();
    }
    let frames: Vec<&[f32]> = a[..a.len() / n * n].chunks_exact(n).collect();
    let lvl: Vec<f32> = frames
        .iter()
        .map(|f| {
            let sq: Vec<f32> = f.iter().map(|x| x * x).collect();
            10.0f32 * (np::mean_f32(&sq) + 1e-12f32).log10()
        })
        .collect();
    let line = np::percentile_f32(&lvl, 90.0).expect("frames") - 20.0f32;
    let win = np::hanning(n);
    let fft = realfft::RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let (mut buf, mut spec) = (fft.make_input_vec(), fft.make_output_vec());
    let mut power = vec![0.0f64; n / 2 + 1];
    let mut count = 0usize;
    for (f, _) in frames.iter().zip(&lvl).filter(|(_, l)| **l > line) {
        for ((b, &x), w) in buf.iter_mut().zip(*f).zip(&win) {
            *b = x as f64 * w;
        }
        fft.process(&mut buf, &mut spec).expect("fft sizes");
        // np.mean(axis=0) over rows adds them one after another
        for (p, c) in power.iter_mut().zip(&spec) {
            *p += c.re.hypot(c.im).powi(2);
        }
        count += 1;
    }
    power.iter_mut().for_each(|p| *p /= count as f64);
    let val = 1.0 / (n as f64 * (1.0 / sr as f64));
    let freqs: Vec<f64> = (0..=n / 2).map(|i| i as f64 * val).collect();
    let k = 2f64.powf(1.0 / 6.0);
    bands()
        .into_iter()
        .map(|f| {
            let sel: Vec<f64> =
                freqs.iter().zip(&power).filter(|(q, _)| **q >= f / k && **q < f * k).map(|(_, p)| *p).collect();
            if !sel.is_empty() && f < sr as f64 / 2.0 * 0.92 {
                10.0 * (np::mean_f64(&sel) + 1e-12).log10()
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// `mic_match(a, sr, ref_bands, max_db)` (Python default max_db = 6.0): a level-neutral, 3-band-smoothed EQ curve
/// (capped at +/- max_db) from the output's to the reference's `ltas`, applied as a 1025-tap linear-phase FIR.
/// Returns the filtered audio and the curve rounded to 0.1 dB; the input and an empty curve when the clip is under
/// 8192 samples, the band counts differ, or fewer than 6 bands are finite on both sides.
pub fn mic_match(a: &[f32], sr: u32, ref_bands: &[f64], max_db: f64) -> (Vec<f32>, Vec<f64>) {
    let out_bands = ltas(a, sr);
    if out_bands.is_empty() || ref_bands.len() != out_bands.len() {
        return (a.to_vec(), Vec::new());
    }
    let mut diff: Vec<f64> = ref_bands.iter().zip(&out_bands).map(|(r, g)| r - g).collect();
    let mut ok: Vec<f64> = diff.iter().copied().filter(|d| d.is_finite()).collect();
    if ok.len() < 6 {
        return (a.to_vec(), Vec::new());
    }
    ok.sort_by(|x, y| x.partial_cmp(y).expect("finite"));
    let h = ok.len() / 2;
    let median = if ok.len() % 2 == 1 { ok[h] } else { (ok[h - 1] + ok[h]) / 2.0 };
    diff.iter_mut().filter(|d| d.is_finite()).for_each(|d| *d -= median);
    let sm: Vec<f64> = (0..diff.len())
        .map(|i| {
            let w: Vec<f64> = diff[i.saturating_sub(1)..(i + 2).min(diff.len())].iter().copied().filter(|d| d.is_finite()).collect();
            let v = if w.is_empty() { 0.0 } else { w.iter().sum::<f64>() / w.len() as f64 };
            v.clamp(-max_db, max_db)
        })
        .collect();
    let nyq = sr as f64 / 2.0;
    // Points below 0.999 x Nyquist are already increasing and distinct, so np.unique keeps them all in order.
    let mut fpts = vec![0.0];
    let mut gains_db = vec![sm[0]];
    for (f, g) in bands().iter().zip(&sm) {
        if *f < nyq * 0.999 {
            fpts.push(f.min(nyq * 0.999) / nyq);
            gains_db.push(*g);
        }
    }
    fpts.push(1.0);
    gains_db.push(0.0);
    let gains: Vec<f64> = gains_db.iter().map(|g| 10f64.powf(g / 20.0)).collect();
    let fir = audio::signal::firwin2(1025, &fpts, &gains);
    let out = audio::signal::fftconvolve_same(a, &fir).into_iter().map(|v| v as f32).collect();
    (out, sm.iter().map(|g| py_round(*g, 1)).collect())
}

/// The `bands` of a `profiles/<id>/mic.json`. Python writes it with plain `json.dumps`, so missing bands are bare
/// `NaN` (not JSON): this reads the list token by token (`NaN`, `Infinity`, `-Infinity` and numbers).
pub fn read_mic_bands(path: &Path) -> Result<Vec<f64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let key = text.find("\"bands\"").ok_or("mic.json has no bands")?;
    let open = key + text[key..].find('[').ok_or("bands is not a list")?;
    let close = open + text[open..].find(']').ok_or("bands list is not closed")?;
    let body = text[open + 1..close].trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    body.split(',').map(|t| t.trim().parse::<f64>().map_err(|e| format!("mic.json band {t:?}: {e}"))).collect()
}
