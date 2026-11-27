//! Voice creation (step S17): backend/app/reference.py. `analyze` cleans an uploaded sample, transcribes it,
//! scores its utterances, builds the voice fingerprint and cuts the reference clips; `refresh_clips` re-cuts a
//! saved voice's clips inside real pauses. The models come in through the `Transcriber` and `SpeakerEncoder`
//! traits (Whisper and the voice encoder in the engine, the scripted fake in tests).
//! Studio restoration (Sidon, S18) is not ported: `STUDIO_RESTORATION` decides, and a studio request runs the
//! plain path with a first tip saying so.

use crate::audio::{self, np::{mean_f64, percentile_f32, py_slice}, noisereduce, PyError};
use crate::checks::py_round;
use crate::delivery::{self, Prosody};
use crate::library::Doc;
use crate::render::models::{self, SpeakerEncoder, Transcriber};
use crate::render::{files, narration, RenderError, CANCELLED};
use crate::text::{py, score};
use crate::transcript::{Segment, Transcript};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// config.py `MAX_REFERENCE_SECONDS` and `WORK_SR`.
pub const MAX_REFERENCE_SECONDS: f64 = 300.0;
pub const WORK_SR: u32 = 24000;
/// Studio restoration (Sidon) is ported: S18 flips this, and `analyze` then needs its studio branch.
pub const STUDIO_RESTORATION: bool = false;
/// The first tip when a studio request ran the plain path.
pub const STUDIO_FALLBACK_TIP: &str = "Studio restoration is not in TakeTwo 1.0 yet, so the plain clean-up was used (high-pass, noise reduction when needed, loudness).";

pub type Progress<'a> = &'a mut dyn FnMut(f64, &str);

fn py_err(e: PyError) -> RenderError {
    if e.kind == "ValueError" { RenderError::Value(e.message) } else { RenderError::Other(format!("{}: {}", e.kind, e.message)) }
}

fn model_err(e: String, cancel: &AtomicBool) -> RenderError {
    if e == CANCELLED || cancel.load(Ordering::SeqCst) { RenderError::Cancelled } else { RenderError::Other(e) }
}

/// `ml.transcribe(a, sr, language, words)`: resampled to 16 kHz first; an empty language means detect.
fn transcribe(tr: &mut dyn Transcriber, a: &[f32], sr: u32, language: Option<&str>, words: bool, cancel: &AtomicBool) -> Result<Transcript, RenderError> {
    let x = audio::resample(a, sr, 16000);
    tr.transcribe(&x, language.filter(|l| !l.is_empty()), words, cancel).map_err(|e| model_err(e, cancel))
}

/// One utterance (`_utterances`' dict, then what `analyze` adds).
#[derive(Clone, Debug, PartialEq)]
pub struct Utterance {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub word_p: f64,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
    pub n_words: usize,
    pub dur: f64,
    pub snr: f64,
    pub clip: f64,
    pub emb: Option<Vec<f32>>,
    pub sim: Option<f64>,
    pub score: f64,
}

/// `_utterances(segments, total)`: words regrouped into utterances split at pauses >= 0.35 s or sentence ends.
pub fn utterances(segments: &[Segment], total: f64) -> Vec<Utterance> {
    let mut groups: Vec<Vec<(usize, &crate::transcript::Word)>> = Vec::new();
    let mut cur: Vec<(usize, &crate::transcript::Word)> = Vec::new();
    for (si, s) in segments.iter().enumerate() {
        for w in &s.words {
            if let (Some(first), Some(last)) = (cur.first(), cur.last()) {
                let gap = w.start - last.1.end;
                let w_last = py::strip(&last.1.word);
                let ends_sentence = w_last.ends_with('.') || w_last.ends_with('!') || w_last.ends_with('?');
                let dur = last.1.end - first.1.start;
                if gap >= 0.35 || (ends_sentence && gap >= 0.12) || dur > 14.0 {
                    groups.push(std::mem::take(&mut cur));
                }
            }
            cur.push((si, w));
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups
        .iter()
        .map(|u| {
            let s0 = &segments[u[0].0];
            let text: String = u.iter().map(|(_, w)| w.word.as_str()).collect();
            let ps: Vec<f64> = u.iter().map(|(_, w)| w.p).collect();
            Utterance {
                start: 0.0f64.max(u[0].1.start),
                end: if total < u[u.len() - 1].1.end { total } else { u[u.len() - 1].1.end },
                text: py::strip(&text).to_string(),
                word_p: mean_f64(&ps),
                avg_logprob: s0.avg_logprob,
                no_speech_prob: s0.no_speech_prob,
                n_words: u.len(),
                dur: 0.0,
                snr: 0.0,
                clip: 0.0,
                emb: None,
                sim: None,
                score: 0.0,
            }
        })
        .collect()
}

/// `pause_threshold(db10)`: the level below which a 10 ms frame is a real pause (float32 arithmetic).
pub fn pause_threshold(db10: &[f32]) -> Result<f64, RenderError> {
    let p10 = percentile_f32(db10, 10.0).map_err(py_err)?;
    let p90 = percentile_f32(db10, 90.0).map_err(py_err)?;
    let (a, b) = (p10 + 10.0, p90 - 20.0);
    Ok(if b < a { b } else { a } as f64)
}

/// `_valleys(db10, thr, min_frames=6)`: runs of frames under `thr` at least `min_frames` long, as [a, b).
pub fn valleys(db10: &[f32], thr: f64, min_frames: usize) -> Vec<(usize, usize)> {
    let below = |i: usize| db10[i] < thr as f32;
    let (mut out, mut i, n) = (Vec::new(), 0, db10.len());
    while i < n {
        if below(i) {
            let mut j = i;
            while j < n && below(j) {
                j += 1;
            }
            if j - i >= min_frames {
                out.push((i, j));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn max_f32(v: &[f32]) -> f32 {
    v.iter().cloned().fold(f32::NEG_INFINITY, |m, x| if x > m || x.is_nan() { x } else { m })
}

/// `_end_pauses(db10, thr, next_start)`: pauses a clip may end in (a stop consonant's short closure is not one).
pub fn end_pauses(db10: &[f32], thr: f64, next_start: Option<f64>) -> Result<Vec<(usize, usize)>, RenderError> {
    let vs = valleys(db10, thr, 6);
    let audible = percentile_f32(db10, 90.0).map_err(py_err)? as f64 - 30.0;
    let mut pauses = Vec::new();
    for (k, &(a, b)) in vs.iter().enumerate() {
        let resume_end = if k + 1 < vs.len() { vs[k + 1].0 } else { db10.len() };
        let next_word = next_start.is_some_and(|n| b as f64 * 0.01 >= n - 0.05);
        let final_sound = b - a <= 20 && resume_end > b && resume_end - b <= 30 && max_f32(&db10[b..resume_end]) as f64 >= audible;
        if next_word || !final_sound {
            pauses.push((a, b));
        }
    }
    Ok(pauses)
}

/// `snap_window(db10, start_t, end_t, next_start, total)`: the clip's cut points moved into real pauses, or None.
pub fn snap_window(db10: &[f32], start_t: f64, end_t: f64, next_start: Option<f64>, total: f64) -> Result<Option<(f64, f64)>, RenderError> {
    let thr = pause_threshold(db10)?;
    let vs = valleys(db10, thr, 6);
    let fs = 0.01;
    // Python tests `next_start` for truth here (0.0 counts as none), and `is not None` in _end_pauses
    let other = match next_start.filter(|n| *n != 0.0) {
        Some(n) => n - 0.02,
        None => total,
    };
    let limit = if other < end_t + 0.7 { other } else { end_t + 0.7 };
    let mut end = None;
    for (a, b) in end_pauses(db10, thr, next_start)? {
        if a as f64 * fs >= end_t - 0.10 && a as f64 * fs <= limit {
            let x = 0.65 * (b - a) as f64 * fs;
            end = Some(a as f64 * fs + if x < 0.22 { x } else { 0.22 });
            break;
        }
    }
    let Some(end) = end else { return Ok(None) };
    let mut start = None;
    if start_t <= 0.3 {
        start = Some(0.0);
    } else {
        for &(a, b) in vs.iter().rev() {
            if b as f64 * fs <= start_t + 0.10 && b as f64 * fs >= start_t - 0.6 {
                let x = 0.5 * (b - a) as f64 * fs;
                start = Some(b as f64 * fs - if x < 0.12 { x } else { 0.12 });
                break;
            }
        }
    }
    match start {
        Some(s) if end - s >= 3.0 && end <= total => Ok(Some((s.max(0.0), end))),
        _ => Ok(None),
    }
}

/// `room_echo(a, sr)`: how loud the gaps are next to speech (dB, float32 percentiles).
pub fn room_echo(a: &[f32], sr: u32) -> Result<f64, RenderError> {
    let db = audio::frame_db(a, sr, audio::FRAME_MS);
    Ok((percentile_f32(&db, 10.0).map_err(py_err)? - percentile_f32(&db, 90.0).map_err(py_err)?) as f64)
}

/// `np.median` of float64 values.
fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 { s[n / 2] } else { (s[n / 2 - 1] + s[n / 2]) / 2.0 }
}

fn clip01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// `np.mean(embs, axis=0)` then `/= np.linalg.norm(c) + 1e-9`, all in float32.
/// shortcut: sums run row by row and the norm's dot product in order; BLAS may order a long sum differently (last bit).
fn centre(embs: &[&Vec<f32>]) -> Vec<f32> {
    let mut c = vec![0.0f32; embs[0].len()];
    for e in embs {
        for (x, y) in c.iter_mut().zip(e.iter()) {
            *x += y;
        }
    }
    let n = embs.len() as f32;
    for x in c.iter_mut() {
        *x /= n;
    }
    let norm = c.iter().map(|x| x * x).sum::<f32>().sqrt() + 1e-9f32;
    for x in c.iter_mut() {
        *x /= norm;
    }
    c
}

/// One reference window: utterances i..=j, its quality, delivery rank, prosody and snapped cut points.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub i: usize,
    pub j: usize,
    pub q: f64,
    pub expr: f64,
    pub pro: Prosody,
    pub snapped: (f64, f64),
}

/// `_candidate_windows(utts, target, limit)`: contiguous spans near `target` seconds, best first, not overlapping.
pub fn candidate_windows(utts: &[Utterance], target: f64, limit: usize) -> Vec<(usize, usize, f64)> {
    let (lo, hi) = (5.0f64.max(target * 0.6), target * 1.5);
    let mut cands = Vec::new();
    for i in 0..utts.len() {
        for j in i..utts.len() {
            let dur = utts[j].end - utts[i].start;
            if dur > hi {
                break;
            }
            if dur < lo {
                continue;
            }
            let span = &utts[i..=j];
            let gaps: Vec<f64> = span.windows(2).map(|w| w[1].start - w[0].end).collect();
            if gaps.iter().any(|g| *g > 1.2) {
                continue; // a long pause inside the prompt teaches the model to pause
            }
            let scores: Vec<f64> = span.iter().map(|u| u.score).collect();
            let q = mean_f64(&scores);
            let worst = scores.iter().cloned().fold(f64::INFINITY, |m, x| if x < m { x } else { m });
            let t = py::rstrip(&span[span.len() - 1].text);
            let ends_clean = (t.ends_with('.') || t.ends_with('!') || t.ends_with('?')) as i32 as f64;
            let s = q + 0.3 * worst + 0.05 * ends_clean - 0.02 * (dur - target).abs();
            cands.push((i, j, s));
        }
    }
    cands.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)); // stable, by -score
    let mut picked: Vec<(usize, usize, f64)> = Vec::new();
    for c in cands {
        if picked.iter().all(|p| c.1 < p.0 || c.0 > p.1) {
            picked.push(c);
        }
        if picked.len() >= limit {
            break;
        }
    }
    if picked.is_empty() && !utts.is_empty() {
        let scores: Vec<f64> = utts.iter().map(|u| u.score).collect();
        picked = vec![(0, utts.len() - 1, mean_f64(&scores))]; // very short sample: use everything
    }
    picked
}

/// `_select_windows(utts, target, a, sr, n=4, db10, total)`: best-quality clips cut in real pauses, with the most
/// expressive and the calmest clean deliveries among them.
pub fn select_windows(utts: &[Utterance], target: f64, a: &[f32], sr: u32, db10: &[f32], total: f64) -> Result<Vec<Window>, RenderError> {
    let n = 4;
    let mut scored: Vec<Window> = Vec::new();
    for (i, j, q) in candidate_windows(utts, target, 40) {
        let nxt = utts.get(j + 1).map(|u| u.start);
        let Some(snapped) = snap_window(db10, utts[i].start, utts[j].end, nxt, total)? else { continue };
        let seg = &a[py_slice(a.len(), (snapped.0 * sr as f64) as i64, (snapped.1 * sr as f64) as i64)];
        let pro = delivery::prosody(seg, sr);
        scored.push(Window { i, j, q, expr: delivery::expressiveness(&pro), pro, snapped });
        if scored.len() >= 14 {
            break;
        }
    }
    if scored.is_empty() {
        return Ok(Vec::new());
    }
    // argsort().argsort(): each value's rank. shortcut: a stable sort; numpy's quicksort may order exact ties differently
    let mut order: Vec<usize> = (0..scored.len()).collect();
    order.sort_by(|&x, &y| scored[x].expr.partial_cmp(&scored[y].expr).unwrap_or(std::cmp::Ordering::Equal));
    let denom = (scored.len() - 1).max(1) as f64;
    let many = scored.len() > 1;
    for (r, &k) in order.iter().enumerate() {
        scored[k].expr = if many { r as f64 / denom } else { 0.5 };
    }
    // Python's max/min keep the first of equal values
    let first_by = |idx: &[usize], key: &dyn Fn(&Window) -> f64, more: bool| -> usize {
        let mut best = idx[0];
        for &k in &idx[1..] {
            let (v, b) = (key(&scored[k]), key(&scored[best]));
            if (more && v > b) || (!more && v < b) {
                best = k;
            }
        }
        best
    };
    let all: Vec<usize> = (0..scored.len()).collect();
    let best_q = scored.iter().map(|c| c.q).fold(f64::NEG_INFINITY, |m, x| if x > m { x } else { m });
    let mut good: Vec<usize> = all.iter().cloned().filter(|&k| scored[k].q >= best_q - 0.2).collect();
    if good.is_empty() {
        good = all.clone();
    }
    let mut picks = vec![first_by(&all, &|c| c.q, true)];
    for extreme in [first_by(&good, &|c| c.expr, true), first_by(&good, &|c| c.expr, false)] {
        if !picks.contains(&extreme) {
            picks.push(extreme);
        }
    }
    let mut by_q = all.clone();
    by_q.sort_by(|&x, &y| scored[y].q.partial_cmp(&scored[x].q).unwrap_or(std::cmp::Ordering::Equal));
    for k in by_q {
        if picks.len() >= n {
            break;
        }
        if !picks.contains(&k) {
            picks.push(k);
        }
    }
    Ok(picks.into_iter().map(|k| scored[k].clone()).collect())
}

/// `_tips(total, snr, self_sim, good_seconds, echo, studio_mode)`.
pub fn tips(total: f64, snr: f64, self_sim: f64, good_seconds: f64, echo: f64, studio_mode: bool) -> Vec<String> {
    let mut t = Vec::new();
    if echo > -30.0 && !studio_mode {
        t.push("This recording has noticeable room echo or background noise, which clones can pick up as a robotic or echoey sound. Turn on Studio quality to remove it.");
    }
    if total < 20.0 {
        t.push("Short sample: 1-5 minutes of clean speech gives a better voice fingerprint and more reference options.");
    }
    if snr < 25.0 && !studio_mode {
        t.push("Background noise detected and reduced. A quieter recording will clone more accurately.");
    }
    if self_sim < 0.6 {
        t.push("The sample seems to contain more than one voice, or music. Upload only the target speaker for best results.");
    }
    if good_seconds < 8.0 {
        t.push("Very little clean speech found. Record in a quiet room, 15-30 cm from the mic.");
    }
    t.into_iter().map(String::from).collect()
}

/// `round(v, 3)` for a float, the value itself otherwise (analysis.json's utterances).
fn r3(x: f64) -> Value {
    Value::from(py_round(x, 3))
}

impl Utterance {
    /// The utterance as analysis.json stores it: Python's key order, floats rounded to 3 places, `emb` left out.
    pub fn doc(&self) -> Value {
        json!({
            "start": r3(self.start), "end": r3(self.end), "text": self.text, "word_p": r3(self.word_p),
            "avg_logprob": r3(self.avg_logprob), "no_speech_prob": r3(self.no_speech_prob), "n_words": self.n_words,
            "dur": r3(self.dur), "snr": r3(self.snr), "clip": r3(self.clip), "sim": self.sim.map_or(Value::Null, r3), "score": r3(self.score),
        })
    }
}

/// The models and the cancel flag `analyze` and `refresh_clips` use.
pub struct Models<'a> {
    pub transcriber: &'a mut dyn Transcriber,
    pub encoder: &'a mut dyn SpeakerEncoder,
    pub cancel: &'a AtomicBool,
}

/// `analyze(src_path, out_dir, language, denoise, target_window, progress, studio_mode)`: the analysis dict, also
/// written to `analysis.json` with the clips, `clean.wav`, `fingerprint.npy` and `mic.json`.
#[allow(clippy::too_many_arguments)]
pub fn analyze(src: &Path, out_dir: &Path, language: Option<&str>, denoise: bool, target_window: f64, progress: Progress, studio_mode: bool,
               m: &mut Models) -> Result<Doc, RenderError> {
    let studio_asked = studio_mode;
    let studio_mode = studio_mode && STUDIO_RESTORATION;
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    progress(0.05, "Decoding audio");
    let clean_path = out_dir.join("clean.wav");
    audio::ffmpeg::decode_to_wav(src, &clean_path, WORK_SR, Some(MAX_REFERENCE_SECONDS))?;
    let (mut a, sr) = audio::load(&clean_path, None)?;
    let total = a.len() as f64 / sr as f64;
    if total < 3.0 {
        return Err(RenderError::Value("Sample is too short. Upload at least 3 seconds (30 s - 5 min recommended).".into()));
    }
    let echo_before = room_echo(&a, sr)?;
    let echo_after: Option<f64> = None;
    let snr_before = audio::estimate_snr(&audio::highpass(&a, sr, 70.0).map_err(py_err)?, sr);
    if studio_mode {
        return Err(RenderError::Other("Studio restoration (Sidon) is not ported yet.".into())); // S18
    }
    let mic_bands = delivery::ltas(&a, sr); // tonal fingerprint the output should match
    a = audio::highpass(&a, sr, 70.0).map_err(py_err)?;
    let denoised = !studio_mode && (denoise || snr_before < 25.0);
    if denoised {
        progress(0.1, "Reducing background noise");
        a = noisereduce::reduce_noise(&a, sr)?;
    }
    a = audio::normalize_lufs(&a, sr, -20.0, -1.5);
    audio::save(&clean_path, &a, sr).map_err(|e| format!("{}: {e}", clean_path.display()))?;

    progress(0.2, "Transcribing the full sample (Whisper large-v3-turbo)");
    let tr = transcribe(m.transcriber, &a, sr, language, true, m.cancel)?;
    let mut utts = utterances(&tr.segments, total);
    if utts.is_empty() {
        return Err(RenderError::Value("No speech found in the sample.".into()));
    }

    progress(0.55, "Building voice fingerprint");
    let nu = utts.len();
    for (i, u) in utts.iter_mut().enumerate() {
        let seg = &a[py_slice(a.len(), (u.start * sr as f64) as i64, (u.end * sr as f64) as i64)];
        u.dur = u.end - u.start;
        u.snr = if seg.len() as f64 > sr as f64 * 0.3 { audio::estimate_snr(seg, sr) } else { 0.0 };
        u.clip = audio::clipping_ratio(seg);
        u.emb = if u.dur >= 1.2 { Some(m.encoder.embedding(seg, sr).map_err(|e| model_err(e, m.cancel))?) } else { None };
        progress(0.55 + 0.25 * (i + 1) as f64 / nu as f64, "Building voice fingerprint");
    }
    let whole;
    let mut embs: Vec<&Vec<f32>> = utts.iter().filter_map(|u| u.emb.as_ref()).collect();
    if embs.is_empty() {
        whole = m.encoder.embedding(&a, sr).map_err(|e| model_err(e, m.cancel))?;
        embs = vec![&whole];
    }
    let mut centroid = centre(&embs);
    // second pass: re-centre on the dominant speaker (drop outliers)
    let sims: Vec<f64> = embs.iter().map(|e| models::cosine(e, &centroid)).collect::<Result<_, _>>()?;
    let med = median(&sims);
    let keep: Vec<&Vec<f32>> = embs.iter().zip(&sims).filter(|(_, s)| **s >= med - 0.1).map(|(e, _)| *e).collect();
    if !keep.is_empty() {
        centroid = centre(&keep);
    }
    let mut new_sims = Vec::new();
    for u in &utts {
        new_sims.push(u.emb.as_ref().map(|e| models::cosine(e, &centroid)).transpose()?);
    }
    for (u, s) in utts.iter_mut().zip(new_sims) {
        u.sim = s;
    }
    let good_sims: Vec<f64> = utts.iter().filter_map(|u| u.sim).collect();
    let self_sim = if good_sims.is_empty() { 0.75 } else { median(&good_sims) };

    for u in utts.iter_mut() {
        let clarity = clip01((u.word_p - 0.5) / 0.45);
        let noise = clip01((u.snr - 20.0) / 25.0);
        let spk = clip01((u.sim.unwrap_or(self_sim - 0.1) - (self_sim - 0.15)) / 0.2);
        let clip_pen = if u.clip > 0.001 { 1.0 } else { 0.0 };
        let no_speech = if u.no_speech_prob > 0.5 { 1.0 } else { 0.0 };
        u.score = 0.35 * clarity + 0.25 * noise + 0.4 * spk - 0.5 * clip_pen - 0.5 * no_speech;
    }

    progress(0.85, "Measuring delivery and choosing reference clips");
    let db10 = audio::frame_db(&a, sr, 10);
    let windows = select_windows(&utts, target_window, &a, sr, &db10, total)?;
    if windows.is_empty() {
        return Err(RenderError::Value("Could not find a clean stretch of speech. Try a clearer recording.".into()));
    }
    let mut variants: Vec<Doc> = Vec::new();
    for (w_i, w) in windows.iter().enumerate() {
        let (start, end) = w.snapped;
        let clip = audio::fade(&a[py_slice(a.len(), (start * sr as f64) as i64, (end * sr as f64) as i64)], sr, 15, 20);
        let name = format!("ref_{w_i}.wav");
        let path = out_dir.join(&name);
        audio::save(&path, &clip, sr).map_err(|e| format!("{}: {e}", path.display()))?;
        let text: Vec<&str> = utts[w.i..=w.j].iter().map(|u| u.text.as_str()).collect();
        let delivery = if w.expr >= 0.67 { "expressive" } else if w.expr <= 0.33 { "calm" } else { "balanced" };
        let v = json!({
            "index": w_i, "file": name, "start": py_round(start, 2), "end": py_round(end, 2), "duration": py_round(end - start, 2),
            "score": py_round(w.q, 3), "text": py::strip(&text.join(" ")), "expressiveness": py_round(w.expr, 3),
            "pitch_range": py_round(w.pro.pitch_range, 1), "delivery": delivery,
        });
        variants.push(v.as_object().expect("an object").clone());
    }
    // best quality first (the rounded score; a stable sort), then a two-pass rename so no clip overwrites another
    variants.sort_by(|x, y| y["score"].as_f64().partial_cmp(&x["score"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    for (k, v) in variants.iter_mut().enumerate() {
        let src = out_dir.join(v["file"].as_str().expect("a name"));
        v.insert("index".into(), k.into());
        v.insert("file".into(), format!("ref_{k}.wav").into());
        std::fs::rename(&src, out_dir.join(format!("tmp_{k}.wav"))).map_err(|e| format!("{}: {e}", src.display()))?;
    }
    for k in 0..variants.len() {
        std::fs::rename(out_dir.join(format!("tmp_{k}.wav")), out_dir.join(format!("ref_{k}.wav"))).map_err(|e| e.to_string())?;
    }

    files::write_npy_f32(&out_dir.join("fingerprint.npy"), &centroid)?;
    std::fs::write(out_dir.join("mic.json"), narration::mic_json(sr, &mic_bands)).map_err(|e| e.to_string())?;
    let good_seconds: f64 = utts.iter().filter(|u| u.score > 0.5).fold(0.0, |s, u| s + u.dur);
    let mut tip_list = tips(total, snr_before, self_sim, good_seconds, echo_before, studio_mode);
    if studio_asked && !studio_mode {
        tip_list.insert(0, STUDIO_FALLBACK_TIP.to_string());
    }
    let lang = if tr.language.is_empty() { language.map_or(Value::Null, Value::from) } else { Value::from(tr.language.as_str()) };
    let analysis = json!({
        "duration": py_round(total, 2),
        "language": lang,
        "snr_db": py_round(snr_before, 1),
        "denoised": denoised,
        "studio": studio_mode, "room_echo_db": py_round(echo_before, 1),
        "room_echo_after_db": echo_after.map_or(Value::Null, |e| py_round(e, 1).into()),
        "self_similarity": py_round(self_sim, 3),
        "usable_speech_seconds": py_round(good_seconds, 1),
        "transcript": tr.text,
        "utterances": utts.iter().map(Utterance::doc).collect::<Vec<_>>(),
        "variants": variants,
        "tips": tip_list,
    });
    files::save_json(&out_dir.join("analysis.json"), &analysis)?;
    progress(1.0, "Done");
    Ok(analysis.as_object().expect("an object").clone())
}

fn num(v: &Value, k: &str) -> Result<f64, RenderError> {
    Ok(py::num(v.get(k).ok_or_else(|| format!("KeyError: '{k}'"))?)?)
}

/// `refresh_clips(pdir, profile)`: re-cut a saved voice's clips inside real pauses from its `clean.wav`, keeping
/// everything the user chose. `profile["variants"]` is updated in place; the report says what changed.
pub fn refresh_clips(pdir: &Path, profile: &mut Doc, m: &mut Models) -> Result<Value, RenderError> {
    let (a, sr) = audio::load(&pdir.join("clean.wav"), None)?;
    let total = a.len() as f64 / sr as f64;
    let analysis: Value = serde_json::from_str(&std::fs::read_to_string(pdir.join("analysis.json")).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let utts = analysis.get("utterances").and_then(Value::as_array).ok_or("KeyError: 'utterances'")?.clone();
    let db10 = audio::frame_db(&a, sr, 10);
    let (mut updated, mut kept, mut trimmed) = (Vec::<Value>::new(), Vec::<Value>::new(), Vec::<Value>::new());
    let language = profile.get("language").filter(|l| py::truthy(l)).map(py::display).unwrap_or_else(|| "en".into());
    let overrides = profile.get("ref_text_override").filter(|o| py::truthy(o)).and_then(Value::as_object).cloned().unwrap_or_default();
    let mut variants = profile.get("variants").filter(|v| py::truthy(v)).and_then(Value::as_array).cloned().unwrap_or_default();
    for v in variants.iter_mut() {
        let v = v.as_object_mut().ok_or("a variant is not an object")?;
        let idx = v.get("index").cloned().ok_or("KeyError: 'index'")?;
        let key = py::display(&idx);
        let (vs, ve) = (num(&Value::Object(v.clone()), "start")?, num(&Value::Object(v.clone()), "end")?);
        let mut inside = Vec::new();
        for u in &utts {
            if num(u, "start")? >= vs - 0.15 && num(u, "end")? <= ve + 0.06 {
                inside.push(u);
            }
        }
        if inside.is_empty() {
            kept.push(idx);
            continue;
        }
        let (first, last) = (inside[0], inside[inside.len() - 1]);
        let last_end = num(last, "end")?;
        let mut nxt = None;
        for u in &utts {
            if num(u, "start")? > last_end + 0.005 {
                nxt = Some(num(u, "start")?);
                break;
            }
        }
        let snapped = snap_window(&db10, num(first, "start")?, last_end, nxt, total)?;
        let v_text = py::display(v.get("text").ok_or("KeyError: 'text'")?);
        let text = overrides.get(&key).filter(|t| py::truthy(t)).map(py::display).unwrap_or_else(|| v_text.clone());
        let mut ok = false;
        let mut cut = None;
        if let Some((s0, e0)) = snapped {
            let clip = audio::fade(&a[py_slice(a.len(), (s0 * sr as f64) as i64, (e0 * sr as f64) as i64)], sr, 15, 20);
            let heard = transcribe(m.transcriber, &clip, sr, Some(&language), false, m.cancel)?.text;
            ok = score::wer(&text, &heard) <= 0.12;
            cut = Some((s0, e0, clip));
        }
        if !ok {
            // Second chance, only for a clip that really ends inside a sound and whose transcript the user hasn't
            // edited: end it at the previous natural pause and take the transcript from what is spoken.
            let thr = pause_threshold(&db10)?;
            let hi = (ve / 0.01) as i64;
            let window = &db10[py_slice(db10.len(), 0.max(hi - 3), hi)];
            if window.is_empty() {
                return Err(RenderError::Value("zero-size array to reduction operation maximum which has no identity".into()));
            }
            let chopped = max_f32(window) > thr as f32;
            let edited = overrides.contains_key(&key);
            let mut back = None;
            if chopped && !edited {
                for (va, vb) in end_pauses(&db10, thr, None)?.into_iter().rev() {
                    if vb as f64 * 0.01 <= ve - 0.15 && va as f64 * 0.01 >= vs + 3.0 {
                        let x = 0.65 * (vb - va) as f64 * 0.01;
                        back = Some(va as f64 * 0.01 + if x < 0.22 { x } else { 0.22 });
                        break;
                    }
                }
            }
            let Some(back) = back else {
                kept.push(idx);
                continue;
            };
            let (s0, e0) = (vs, back);
            let clip = audio::fade(&a[py_slice(a.len(), (s0 * sr as f64) as i64, (e0 * sr as f64) as i64)], sr, 15, 20);
            let heard = transcribe(m.transcriber, &clip, sr, Some(&language), false, m.cancel)?.text;
            let heard = py::strip(&heard).to_string();
            if (py::split(&heard).len() as f64) < 0.6 * py::split(&v_text).len() as f64 {
                kept.push(idx);
                continue;
            }
            v.insert("text".into(), heard.into());
            v.insert("trimmed_last_word".into(), true.into());
            cut = Some((s0, e0, clip));
        }
        let (s0, e0, clip) = cut.expect("set on both paths");
        let path = pdir.join(py::display(v.get("file").ok_or("KeyError: 'file'")?));
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let backup = path.with_file_name(format!("{stem}.v3.wav"));
        if path.exists() && !backup.exists() {
            std::fs::copy(&path, &backup).map_err(|e| format!("{}: {e}", backup.display()))?;
        }
        audio::save(&path, &clip, sr).map_err(|e| format!("{}: {e}", path.display()))?;
        v.insert("start".into(), py_round(s0, 2).into());
        v.insert("end".into(), py_round(e0, 2).into());
        v.insert("duration".into(), py_round(e0 - s0, 2).into());
        v.insert("clip_version".into(), 4.into());
        updated.push(idx.clone());
        if v.get("trimmed_last_word").is_some_and(py::truthy) {
            trimmed.push(idx);
        }
    }
    if profile.get("variants").is_some_and(py::truthy) {
        profile.insert("variants".into(), Value::Array(variants));
    }
    let mut report = json!({"updated": updated, "kept": kept});
    if !trimmed.is_empty() {
        report["trimmed"] = Value::Array(trimmed);
    }
    Ok(report)
}
