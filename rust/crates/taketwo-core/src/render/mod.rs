//! Takes and retakes of one part (step S15): backend/app/pipeline.py `Cancelled`, `profile_assets`, `noise_limit`
//! (file reads and the `put_profile` write-back; the arithmetic is `checks::noise_limit`), `_ref_pitch`,
//! `_guarded_take`, `render_chunk` and `widen_sentence_pauses`. Only the `qwen3` family.
//! The nonce (`secrets.randbits(32)`), the clock (`time.time`), `put_profile` and the models are inputs.
//! Where Python's `evaluate` transcribes for itself, this code transcribes first with the same audio and arguments.
//! Parity vectors: rust/golden/record_render.py -> rust/golden/render.json, tests/render_golden.rs.

pub mod files;
pub mod models;

use crate::audio;
use crate::checks::{self, py_round, Evaluation, Limits, HARD_ISSUES};
use crate::delivery;
use crate::ending_guard;
use crate::library::Doc;
use crate::text::{self, py};
use crate::transcript::{Segment, Transcript, Word};
use models::{SpeakerEncoder, Transcriber, Voice};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const QWEN_KEYS: [&str; 4] = ["temperature", "top_p", "top_k", "repetition_penalty"];
const TWO32: u128 = 1 << 32;

/// What render_chunk raises: `Cancelled`, a `ValueError` with Python's message, or any other failure.
#[derive(Clone, Debug, PartialEq)]
pub enum RenderError {
    Cancelled,
    Value(String),
    Other(String),
}

impl RenderError {
    /// The Python exception type the golden vectors record.
    pub fn kind(&self) -> &'static str {
        match self {
            RenderError::Cancelled => "Cancelled",
            RenderError::Value(_) => "ValueError",
            RenderError::Other(_) => "Error",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            RenderError::Cancelled => "",
            RenderError::Value(m) | RenderError::Other(m) => m,
        }
    }
}

impl From<String> for RenderError {
    fn from(m: String) -> Self {
        RenderError::Other(m)
    }
}

impl From<&str> for RenderError {
    fn from(m: &str) -> Self {
        RenderError::Other(m.into())
    }
}

/// Everything render_chunk reaches outside its arguments. `put_profile` is `db.put_profile`; `clock` is
/// `time.time`; `cancel` is read where Python calls `cancel()` and handed to the models; `pitch_cache` is
/// `_pitch_cache` (keyed on path, mtime in ns and size).
pub struct Render<'a> {
    pub profiles_dir: &'a Path,
    pub generations_dir: &'a Path,
    pub voice: &'a mut dyn Voice,
    pub transcriber: &'a mut dyn Transcriber,
    pub encoder: &'a mut dyn SpeakerEncoder,
    pub put_profile: &'a mut dyn FnMut(&mut Doc) -> Result<(), String>,
    pub clock: &'a mut dyn FnMut() -> f64,
    pub cancel: &'a AtomicBool,
    pub pitch_cache: &'a mut HashMap<String, f64>,
}

impl Render<'_> {
    fn check(&self) -> Result<(), RenderError> {
        if self.cancel.load(Ordering::SeqCst) { Err(RenderError::Cancelled) } else { Ok(()) }
    }

    /// `ml.transcribe(a, sr, language=language, words=True)`: resampled to 16 kHz first, as ml.py does.
    fn transcribe(&mut self, a: &[f32], sr: u32, language: &str) -> Result<Transcript, RenderError> {
        let x = audio::resample(a, sr, 16000);
        let lang = if language.is_empty() { None } else { Some(language) };
        Ok(self.transcriber.transcribe(&x, lang, true, self.cancel)?)
    }

    /// `evaluate(a, sr, text, fp, self_sim, wps, s, language, noise_max, transcript)`.
    #[allow(clippy::too_many_arguments)]
    fn evaluate(&mut self, a: &[f32], sr: u32, text: &str, assets: &Assets, s: &Doc, language: &str, noise_max: f64, transcript: Option<Transcript>) -> Result<Evaluation, RenderError> {
        let limits = Limits {
            max_wer: py::num(get(s, "max_wer")?)?,
            min_speaker_similarity: py::num(get(s, "min_speaker_similarity")?)?,
            max_internal_silence: py::num(get(s, "max_internal_silence")?)?,
        };
        let empty = (a.len() as f64) < sr as f64 * 0.3; // evaluate returns `empty` before it would transcribe
        let tr = match transcript {
            Some(t) => t,
            None if empty => Transcript { text: String::new(), segments: vec![], language: String::new() },
            None => self.transcribe(a, sr, language)?,
        };
        let mut failed = None;
        let encoder = &mut *self.encoder;
        let ev = checks::evaluate(a, sr, text, assets.self_sim, assets.wps, &limits, noise_max, &tr, |x, r| {
            match encoder.embedding(x, r).and_then(|e| models::cosine(&e, &assets.fp)) {
                Ok(v) => v,
                Err(e) => {
                    failed = Some(e);
                    0.0
                }
            }
        });
        match failed {
            Some(e) => Err(RenderError::Other(e)),
            None => Ok(ev),
        }
    }
}

fn get<'a>(d: &'a Doc, k: &str) -> Result<&'a Value, RenderError> {
    d.get(k).ok_or_else(|| RenderError::Other(format!("KeyError: '{k}'")))
}

fn f(x: f64) -> Value {
    Value::from(x)
}

fn opt(x: Option<f64>) -> Value {
    x.map_or(Value::Null, f)
}

/// Python `==` between two JSON values: numbers by value (0 == 0.0), the rest as they are.
fn py_eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if a.is_number() && b.is_number() => x == y,
        _ => a == b,
    }
}

/// `profile_assets`' tuple.
#[derive(Clone, Debug, PartialEq)]
pub struct Assets {
    pub ref_wav: PathBuf,
    pub ref_text: String,
    pub fp: Vec<f32>,
    pub self_sim: f64,
    pub wps: f64,
}

/// `profile_assets(profile, variant)`: `variant` (or `active_variant`, default 0) is a position in `variants`.
pub fn profile_assets(profiles_dir: &Path, profile: &Doc, variant: Option<&Value>) -> Result<Assets, String> {
    let id = py::display(profile.get("id").ok_or("KeyError: 'id'")?);
    let pdir = profiles_dir.join(id);
    let pos = match variant {
        Some(v) => py::int(v)?,
        None => profile.get("active_variant").map_or(Ok(0), py::int)?,
    };
    let vs = profile.get("variants").and_then(Value::as_array).ok_or("KeyError: 'variants'")?;
    // shortcut: a negative position counts from the end as in Python; positions come from the voice's own list
    let i = if pos < 0 { vs.len() as i64 + pos } else { pos };
    let v = vs.get(i as usize).filter(|_| i >= 0).ok_or("IndexError: list index out of range")?;
    let file = v.get("file").and_then(Value::as_str).ok_or("KeyError: 'file'")?;
    let idx = py::display(v.get("index").ok_or("KeyError: 'index'")?);
    let over = profile.get("ref_text_override").filter(|o| py::truthy(o)).and_then(|o| o.get(&idx)).filter(|t| py::truthy(t));
    let ref_text = match over {
        Some(t) => py::display(t),
        None => py::display(v.get("text").ok_or("KeyError: 'text'")?),
    };
    let fp = files::read_npy(&pdir.join("fingerprint.npy"))?;
    let num_or = |k: &str, d: f64| profile.get(k).map_or(Ok(d), py::num);
    Ok(Assets { ref_wav: pdir.join(file), ref_text, fp, self_sim: num_or("self_similarity", 0.8)?, wps: num_or("speech_rate_wps", 2.6)? })
}

/// `noise_limit(profile)`: measures `noise_baseline` once over `clean.wav` and the clips, stores it with
/// `put_profile`, then `checks::noise_limit`.
pub fn noise_limit(profiles_dir: &Path, profile: &mut Doc, put_profile: &mut dyn FnMut(&mut Doc) -> Result<(), String>) -> Result<f64, String> {
    if !profile.contains_key("noise_baseline") {
        let pdir = profiles_dir.join(py::display(profile.get("id").ok_or("KeyError: 'id'")?));
        let mut paths = vec![pdir.join("clean.wav")];
        for v in profile.get("variants").and_then(Value::as_array).into_iter().flatten() {
            paths.push(pdir.join(v.get("file").and_then(Value::as_str).ok_or("KeyError: 'file'")?));
        }
        let mut vals = Vec::new();
        for p in paths {
            if p.exists() {
                let (a, sr) = audio::load(&p, None)?;
                vals.push(audio::noise_bursts(&a, sr));
            }
        }
        profile.insert("noise_baseline".into(), f(checks::noise_baseline(&vals)));
        put_profile(profile)?;
    }
    Ok(checks::noise_limit(py::num(&profile["noise_baseline"])?))
}

/// `_ref_pitch(ref_wav)`: the clip's pitch movement, cached per file revision; 0 is None.
pub fn ref_pitch(cache: &mut HashMap<String, f64>, ref_wav: &Path) -> Result<Option<f64>, String> {
    let st = std::fs::metadata(ref_wav).map_err(|e| format!("{}: {e}", ref_wav.display()))?;
    let ns = st.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let k = format!("{}:{ns}:{}", ref_wav.display(), st.len());
    if !cache.contains_key(&k) {
        let (a, sr) = audio::load(ref_wav, None)?;
        cache.insert(k.clone(), delivery::prosody(&a, sr).pitch_st);
    }
    let p = cache[&k];
    Ok(if p != 0.0 { Some(p) } else { None })
}

/// A word time inside a saved part (`{"word", "start", "end"}`).
#[derive(Clone, Debug, PartialEq)]
pub struct PartWord {
    pub word: String,
    pub start: f64,
    pub end: f64,
}

/// `widen_sentence_pauses(a, sr, text, words, target_s)`: every full stop inside the part gets at least
/// `target_s` of quiet, filled with the take's own room tone; returns the audio and the moved word times.
pub fn widen_sentence_pauses(a: &[f32], sr: u32, text: &str, words: &[PartWord], target_s: f64) -> (Vec<f32>, Vec<PartWord>) {
    // shortcut: char::is_alphanumeric stands for str.isalnum (same on letters and digits; rare numerics differ)
    let norm = |w: &str| -> String { w.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect() };
    let mut ends = BTreeSet::new();
    let mut n = 0usize;
    let sentences = text::split_sentences(text);
    for sentence in &sentences[..sentences.len().saturating_sub(1)] {
        n += py::split(sentence).len();
        ends.insert(n as i64 - 1);
    }
    let x: Vec<String> = py::split(text).into_iter().map(norm).collect();
    let y: Vec<String> = words.iter().map(|w| norm(&w.word)).collect();
    let sm = text::SequenceMatcher::new(&x, &y, false);
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for (i, j, size) in sm.get_matching_blocks() {
        for k in 0..size {
            if ends.contains(&((i + k) as i64)) {
                let w = &words[j + k];
                spans.push((w.end, if j + k + 1 < words.len() { words[j + k + 1].start } else { w.end + 0.4 }));
            }
        }
    }
    spans.sort_by(|p, q| p.partial_cmp(q).expect("word times are numbers"));
    let db = audio::frame_db(a, sr, audio::FRAME_MS);
    if db.is_empty() {
        return (a.to_vec(), words.to_vec()); // shortcut: numpy's percentile of no frames raises; a part always has frames
    }
    // the audible pause: 35 dB under the speech level (float32, as numpy)
    let line = audio::np::percentile_f32(&db, 90.0).expect("frames") - 35.0f32;
    let hop = (sr * audio::FRAME_MS / 1000) as usize;
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (fi, &d) in db.iter().enumerate() {
        let q = d <= line;
        match (q, start) {
            (true, None) => start = Some(fi),
            (false, Some(s0)) => {
                runs.push((s0 * hop, fi * hop)); // runs touching the end are not between words
                start = None;
            }
            _ => {}
        }
    }
    let srf = sr as f64;
    let mut words = words.to_vec();
    let mut out: Vec<f32> = Vec::new();
    let mut last = 0usize;
    let mut widened = false;
    for (t, nxt) in spans {
        let near: Vec<&(usize, usize)> =
            runs.iter().filter(|r| (r.0 as f64) < (nxt + 0.1) * srf && (r.1 as f64) > (t - 0.25) * srf && r.0 > 0).collect();
        let Some(&&first) = near.first() else { continue };
        let (g0, g1) = near.iter().fold(first, |b, r| if r.1 - r.0 > b.1 - b.0 { **r } else { b });
        let need = (target_s * srf) as i64 - (g1 - g0) as i64;
        let mid = (g0 + g1) / 2;
        if need <= 0 || mid < last {
            continue;
        }
        let need = need as usize;
        let w = (0.01 * srf) as usize;
        let src = &a[g0.max(mid.saturating_sub(w))..g1.min(mid + w)];
        out.extend_from_slice(&a[last..mid]);
        out.extend((0..need).map(|i| if src.is_empty() { 0.0 } else { src[i % src.len()] })); // np.resize
        last = mid;
        widened = true;
        let shift = need as f64 / srf;
        for x in words.iter_mut() {
            if x.start * srf >= mid as f64 {
                x.start += shift;
                x.end += shift;
            }
        }
    }
    if !widened {
        return (a.to_vec(), words);
    }
    out.extend_from_slice(&a[last..]);
    (out, words)
}

/// An unsafe boundary on the last local retry, with what Python attaches to the exception.
struct Unsafe {
    message: String,
    audio: Vec<f32>,
    sr: u32,
    transcript: Transcript,
    synthesis: Doc,
}

enum GuardError {
    Render(RenderError),
    Unsafe(Box<Unsafe>),
}

impl From<RenderError> for GuardError {
    fn from(e: RenderError) -> Self {
        GuardError::Render(e)
    }
}

struct Guarded {
    raw: Vec<f32>,
    sr: u32,
    transcript: Transcript,
    context: Doc,
    full: Vec<f32>,
    report: Doc,
}

fn plain_transcript(words: Vec<Word>) -> Transcript {
    let text = words.iter().map(|w| py::strip(&w.word)).collect::<Vec<_>>().join(" ");
    // only `text` and the words are read downstream; Python's dict has no other key
    Transcript { text, segments: vec![Segment { start: 0.0, end: 0.0, text: String::new(), avg_logprob: 0.0, no_speech_prob: 0.0, words }], language: String::new() }
}

/// `_guarded_take(text, engine, ref_wav, ref_text, language, params, seed, wps, budget, pause_ms, cancel)`.
#[allow(clippy::too_many_arguments)]
fn guarded_take(r: &mut Render, text: &str, engine: &str, assets: &Assets, language: &str, params: &Doc, seed: u32, budget: f64, pause_ms: f64) -> Result<Guarded, GuardError> {
    let sentences = text::split_sentences(text);
    let (mut parts, mut contexts, mut words, mut diagnostics, mut reports): (Vec<Vec<f32>>, Vec<Doc>, Vec<Word>, Vec<f32>, Vec<Doc>) = Default::default();
    let mut sr_out = None;
    let mut offset = 0.0f64;
    for (si, sentence) in sentences.iter().enumerate() {
        r.check()?;
        let (speak, _) = ending_guard::synthesis_text(sentence, "qwen3", language, true);
        let per = text::word_count(&speak) as f64 / assets.wps;
        let expected = ((if 1.0 >= per { 1.0 } else { per }) + 0.4) * budget;
        let mut boundary_attempts: Vec<Value> = Vec::new();
        // within an old multi-sentence part, keep the sentences already separated
        let local_limit = if sentences.len() > 1 { 4 } else { 1 };
        let mut kept = None;
        for retry in 0..local_limit {
            r.check()?;
            let sentence_seed = ((seed as u128 + si as u128 * 2654435761 + retry as u128 * 2246822519) % TWO32) as u32;
            let take = r.voice.synthesize(engine, &speak, &assets.ref_wav, &assets.ref_text, language, params, sentence_seed, expected, r.cancel).map_err(RenderError::Other)?;
            r.check()?;
            let tr = r.transcribe(&take.audio, take.sr, language)?;
            let all: Vec<Word> = tr.segments.iter().flat_map(|s| s.words.iter().cloned()).collect();
            match ending_guard::remove_context(&take.audio, take.sr, sentence, &all) {
                Err(exc) => {
                    boundary_attempts.push(json!({"seed": sentence_seed, "error": exc.0, "synthesis": take.report}));
                    if retry == local_limit - 1 {
                        return Err(GuardError::Unsafe(Box::new(Unsafe { message: exc.0.into(), audio: take.audio, sr: take.sr, transcript: tr, synthesis: take.report })));
                    }
                }
                Ok(removed) => {
                    boundary_attempts.push(json!({"seed": sentence_seed, "synthesis": take.report}));
                    kept = Some((take, removed, sentence_seed));
                    break;
                }
            }
        }
        let (take, removed, sentence_seed) = kept.expect("the last retry returns or raises");
        let sr = take.sr;
        let so = *sr_out.get_or_insert(sr);
        // no crossfade or fade over either side of the sentence
        let retained = audio::resample(&removed.audio, sr, so);
        diagnostics.extend(audio::resample(&take.audio, sr, so));
        if si > 0 {
            let prev_tail = audio::edge_silence(parts.last().expect("a sentence before"), so).1;
            let head = audio::edge_silence(&retained, so).0;
            let g = pause_ms / 1000.0 - prev_tail - head;
            let gap = vec![0.0f32; ((if 0.0 >= g { 0.0 } else { g }) * so as f64).round_ties_even() as usize];
            offset += gap.len() as f64 / so as f64;
            parts.push(gap);
        }
        words.extend(removed.words.iter().map(|w| Word { start: w.start + offset, end: w.end + offset, ..w.clone() }));
        let mut c = Doc::new();
        for (k, v) in [
            ("version", Value::from(removed.version)),
            ("continuation", ending_guard::CONTINUATION.into()),
            ("full_duration", f(removed.full_duration)),
            ("cut_seconds", f(removed.cut_seconds)),
            ("pause_start", f(removed.pause_start)),
            ("pause_end", f(removed.pause_end)),
            ("context_start", f(removed.context_start)),
            ("last_word_end", f(removed.last_word_end)),
            ("threshold_db", f(removed.threshold_db)),
            ("sentence", si.into()),
            ("offset", f(offset)),
            ("seed", sentence_seed.into()),
            ("expected_seconds", f(expected)),
            ("boundary_attempts", Value::Array(boundary_attempts)),
        ] {
            c.insert(k.into(), v);
        }
        contexts.push(c);
        reports.push(take.report);
        offset += retained.len() as f64 / so as f64;
        parts.push(retained);
    }
    let multi = |list: &Vec<Doc>| -> Doc {
        let mut d = list.last().expect("one sentence at least").clone();
        if list.len() > 1 {
            d.insert("sentences".into(), Value::Array(list.iter().cloned().map(Value::Object).collect()));
        }
        d
    };
    let context = multi(&contexts);
    let mut report = multi(&reports);
    if reports.iter().any(|x| x.get("stop_reason") == Some(&Value::from("length_limit_reached"))) {
        report.insert("stop_reason".into(), "length_limit_reached".into());
    }
    Ok(Guarded { raw: parts.concat(), sr: sr_out.unwrap_or(24000), transcript: plain_transcript(words), context, full: diagnostics, report })
}

/// A scored take: `evaluate`'s dict and what render_chunk adds to it.
struct Cand {
    ev: Evaluation,
    context: Option<Doc>,
    full_raw: Option<Vec<f32>>,
    seed: u32,
    pitch_hz: Option<f64>,
    pick: f64,
    has_hard: bool,
}

fn is_hard(i: &str) -> bool {
    HARD_ISSUES.contains(&i)
}

fn ending_doc(e: &checks::Ending) -> Value {
    let mut d = Doc::new();
    d.insert("start".into(), f(e.start));
    d.insert("cut".into(), f(e.cut));
    d.insert("natural".into(), e.natural.into());
    d.insert("junk".into(), e.junk.into());
    d.insert("last_level".into(), f(e.last_level));
    if let (Some(t), Some(h)) = (e.trim_start, e.head_pad_s) {
        d.insert("trim_start".into(), f(t));
        d.insert("head_pad_s".into(), f(h));
    }
    d.insert("raw_duration".into(), f(e.raw_duration));
    d.insert("last_word_end".into(), f(e.last_word_end));
    Value::Object(d)
}

fn issues_value(v: &[&str]) -> Value {
    Value::Array(v.iter().map(|&i| i.into()).collect())
}

/// `render_chunk(narration, ci, profile, s, regen_round, cancel)`: record takes of part `ci` until one is clean
/// (or `compare_takes` are), keep the best, write `chunk_NNN.wav`, `raw_chunk_NNN.wav` and the context evidence,
/// and update the part in `narration`. Returns the updated part. `nonce` is Python's `secrets.randbits(32)`.
pub fn render_chunk(r: &mut Render, narration: &mut Doc, ci: usize, profile: &mut Doc, s: &Doc, regen_round: i64, nonce: u32) -> Result<Doc, RenderError> {
    let engine = py::display(get(narration, "engine")?);
    let family = match engine.as_str() {
        "qwen3-tts-1.7b" | "qwen3-tts-0.6b" => "qwen3",
        // the omnivoice `length_scale` retry branch is not ported: other families are not in TakeTwo 1.0
        _ => return Err(RenderError::Other(format!("Engine {engine} is not in TakeTwo 1.0: only Qwen3-TTS renders."))),
    };
    let language = py::display(get(narration, "language")?);
    let gid = py::display(get(narration, "id")?);
    let chunks = narration.get("chunks").and_then(Value::as_array).ok_or("KeyError: 'chunks'")?.clone();
    let ch = chunks.get(ci).and_then(Value::as_object).ok_or("IndexError: list index out of range")?.clone();
    let ch_text = py::display(get(&ch, "text")?);
    let mut vs = delivery::default_voice_settings();
    if let Some(Value::Object(v)) = narration.get("voice_settings") {
        vs.extend(v.clone());
    }
    let base = match narration.get("params") {
        Some(Value::Object(p)) => p.clone(),
        _ => Doc::new(),
    };
    let (params, qc) = delivery::resolve(Some(&vs), &QWEN_KEYS, Some(&base), s)?;
    let mut s = s.clone();
    s.extend(qc);
    let style = py::num(get(&vs, "style")?)?;
    let mut vidx = delivery::pick_variant(profile, style)?;
    let mut assets = profile_assets(r.profiles_dir, profile, Some(&vidx))?;
    let nmax = noise_limit(r.profiles_dir, profile, &mut *r.put_profile)?;
    let mut ref_pitch = ref_pitch(r.pitch_cache, &assets.ref_wav)?; // always: monotone takes are retaken, lively ones preferred
    // pitch level must stay near the voice's own median and the neighbouring kept parts
    let mut pitch_targets: Vec<f64> = Vec::new();
    if let Some(p) = profile.get("pitch_hz").filter(|p| py::truthy(p)) {
        pitch_targets.push(py::num(p)?);
        let me = py::num(get(&ch, "i")?)?;
        for c in &chunks {
            let c = c.as_object().ok_or("a part is not an object")?;
            if (py::num(get(c, "i")?)? - me).abs() == 1.0 && c.get("file").is_some_and(py::truthy) {
                if let Some(h) = c.get("pitch_hz").filter(|h| py::truthy(h)) {
                    pitch_targets.push(py::num(h)?);
                }
            }
        }
    }
    let gdir = r.generations_dir.join(&gid);
    let base_temp = params.get("temperature").filter(|t| !t.is_null()).map(py::num).transpose()?;
    let mut best: Option<Cand> = None;
    let mut attempts: Vec<Value> = Vec::new();
    let mut passed = 0;
    // keep learned tail room on a later Re-speak/Resume; still bounded against runaways
    let b = ch.get("duration_budget").map_or(Ok(1.0), py::num)?;
    let mut budget = { let x = if 1.0 >= b { 1.0 } else { b }; if 4.0 <= x { 4.0 } else { x } };
    let compare = py::int(s.get("compare_takes").unwrap_or(&Value::from(1)))?.max(1);
    let max_takes = py::int(get(&s, "max_attempts")?)? + compare - 1;
    let mut used: BTreeSet<u32> = BTreeSet::new();
    for v in ch.get("used_seeds").and_then(Value::as_array).into_iter().flatten() {
        used.insert(py::int(v)? as u32);
    }
    for a in ch.get("attempts").and_then(Value::as_array).into_iter().flatten() {
        used.insert(py::int(a.get("seed").ok_or("KeyError: 'seed'")?)? as u32);
    }
    let enabled = s.get("experimental_qwen_ending_guard") == Some(&Value::Bool(true));
    let (speak, guarded) = ending_guard::synthesis_text(&ch_text, family, &language, enabled);
    let s_seed = py::int(get(&s, "seed")?)? as i128;
    let mut seed: u32 = 0;
    let mut p = params.clone();
    let wrap = |x: i128| x.rem_euclid(TWO32 as i128) as u32;
    let next_free = |mut x: u32, used: &BTreeSet<u32>| {
        while used.contains(&x) {
            x = x.wrapping_add(1);
        }
        x
    };
    let pause_ms = py::num(get(&s, "sentence_pause_ms")?)?;
    for k in 0..max_takes.max(0) {
        r.check()?;
        seed = next_free(wrap(s_seed + nonce as i128 + ci as i128 * 1000 + regen_round as i128 * 97 + k as i128), &used);
        used.insert(seed);
        p = params.clone();
        // ending-only failures (cut-off / stray tail) just need a new seed; anything else steadies the model a little
        let n_decay = attempts
            .iter()
            .filter(|a| !a["issues"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|i| is_hard(i)).all(|i| i == "cut_off" || i == "junk_tail"))
            .count();
        if let (Some(t), true) = (base_temp, n_decay > 0) {
            let x = t * 0.9f64.powf(n_decay as f64);
            p.insert("temperature".into(), f(if 0.3 >= x { 0.3 } else { x }));
        }
        let mut others: Vec<&Value> = profile.get("variants").and_then(Value::as_array).into_iter().flatten().filter(|v| !py_eq(&v["index"], &vidx)).collect();
        let score_of = |v: &Value| v.get("score").map_or(Ok(0.0), py::num);
        let mut keyed = Vec::new();
        for v in others.drain(..) {
            keyed.push((-score_of(v)?, v));
        }
        keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("scores are numbers")); // stable, as sorted()
        let all_failed = attempts.iter().all(|a| a["issues"].as_array().into_iter().flatten().filter_map(Value::as_str).any(is_hard));
        if k == max_takes - 1 && k > 0 && !keyed.is_empty() && all_failed {
            // every take so far failed: the last take uses the voice's next-best clip (recorded in "ref")
            vidx = keyed[0].1.get("index").cloned().ok_or("KeyError: 'index'")?;
            assets = profile_assets(r.profiles_dir, profile, Some(&vidx))?;
            ref_pitch = self::ref_pitch(r.pitch_cache, &assets.ref_wav)?;
        }
        let t0 = (r.clock)();
        let per = text::word_count(&speak) as f64 / assets.wps;
        let mut expected_seconds = ((if 1.0 >= per { 1.0 } else { per }) + if guarded { 0.4 } else { 0.0 }) * budget;
        let (raw, sr, transcript, context, full_raw, synthesis) = if guarded {
            match guarded_take(r, &ch_text, &engine, &assets, &language, &p, seed, budget, pause_ms) {
                Ok(g) => {
                    expected_seconds = match g.context.get("sentences").and_then(Value::as_array) {
                        Some(list) => list.iter().map(|c| c["expected_seconds"].as_f64().unwrap_or(0.0)).fold(0.0, |a, b| a + b),
                        None => g.context["expected_seconds"].as_f64().unwrap_or(0.0),
                    };
                    (g.raw, g.sr, Some(g.transcript), Some(g.context), Some(g.full), g.report)
                }
                Err(GuardError::Render(e)) => return Err(e),
                Err(GuardError::Unsafe(exc)) => {
                    // never save or export a spoken continuation, or cut at an ASR guess
                    let secs = py_round((r.clock)() - t0, 1);
                    attempts.push(json!({"seed": seed, "temperature": p.get("temperature").cloned().unwrap_or(Value::Null),
                        "issues": ["ending_boundary"], "context_error": exc.message, "synthesis": exc.synthesis,
                        "expected_seconds": expected_seconds, "secs": secs}));
                    if exc.synthesis.get("stop_reason") == Some(&Value::from("length_limit_reached")) {
                        budget = { let x = budget * 1.3; if 4.0 <= x { 4.0 } else { x } };
                    }
                    if k == max_takes - 1 {
                        // diagnostic only: never assigned to the part or assembled
                        files::write_float_wav(&gdir.join(format!("rejected_context_{ci:03}.wav")), &exc.audio, exc.sr)?;
                        let tr = serde_json::to_value(&exc.transcript).map_err(|e| e.to_string())?;
                        files::save_json(&gdir.join(format!("rejected_context_{ci:03}.json")), &json!({"attempts": attempts, "transcript": tr}))?;
                    }
                    continue;
                }
            }
        } else {
            let take = r.voice.synthesize(&engine, &speak, &assets.ref_wav, &assets.ref_text, &language, &p, seed, expected_seconds, r.cancel)?;
            (take.audio, take.sr, None, None, None, take.report)
        };
        r.check()?;
        let mut ev = r.evaluate(&raw, sr, &ch_text, &assets, &s, &language, nmax, transcript)?;
        r.check()?; // a stop during a long take must not wait for the whole part
        if synthesis.get("stop_reason") == Some(&Value::from("length_limit_reached")) {
            budget = { let x = budget * 1.3; if 4.0 <= x { 4.0 } else { x } };
            if !ev.issues.contains(&"cut_off") {
                ev.issues.push("cut_off");
                ev.score += 2.5;
            }
        }
        let pro = if (ref_pitch.is_some() || !pitch_targets.is_empty()) && ev.dur > 1.5 { Some(delivery::prosody(&ev.audio, sr)) } else { None };
        let (mut liveliness, mut pitch_hz) = (None, None);
        if let (Some(pro), Some(rp)) = (pro, ref_pitch) {
            // keep the reference's pitch movement: flat takes are retaken, livelier ones win (more so with Style up)
            let l = py_round(pro.pitch_st / rp, 2);
            let want = 0.85 + 0.15 * style;
            let gap = want - l;
            ev.score = py_round(ev.score + 1.5 * (if 0.0 >= gap { 0.0 } else { gap }), 4);
            if l < 0.65 {
                ev.issues.push("flat");
            }
            liveliness = Some(l);
        }
        if let Some(pro) = pro.filter(|p| !pitch_targets.is_empty() && p.pitch_hz != 0.0) {
            // pitch level: takes of one voice drifted by up to 5.5 semitones and all passed
            let hz = py_round(pro.pitch_hz, 1);
            let off = pitch_targets.iter().map(|t| (12.0 * (hz / t).log2()).abs()).fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
            ev.score = py_round(ev.score + 0.5 * off, 4); // among failing takes the closest pitch wins
            if off > s.get("max_pitch_offset_st").map_or(Ok(2.0), py::num)? {
                ev.issues.push("off_pitch");
            }
            pitch_hz = Some(hz);
        }
        let hard = ev.issues.iter().any(|i| is_hard(i));
        let secs = py_round((r.clock)() - t0, 1);
        let d = ev.details.as_ref();
        attempts.push(json!({
            "seed": seed, "temperature": p.get("temperature").cloned().unwrap_or(Value::Null), "wer": ev.wer, "sim": ev.sim,
            "max_gap": ev.max_gap, "dur": ev.dur, "issues": issues_value(&ev.issues), "score": ev.score, "secs": secs,
            "liveliness": opt(liveliness), "pitch_hz": opt(pitch_hz), "ref": vidx.clone(),
            "heard": d.map_or("", |d| d.heard.as_str()), "ending": d.map_or(Value::Null, |d| ending_doc(&d.ending)),
            "synthesis": synthesis, "expected_seconds": expected_seconds, "ending_guard": context.clone().map_or(Value::Null, Value::Object),
        }));
        // among clean takes, prefer the one closest to the real voice
        let pick = ev.score - if !hard { 0.6 * ev.sim } else { 0.0 };
        let cand = Cand { ev, context, full_raw: if guarded { full_raw } else { None }, seed, pitch_hz, pick, has_hard: hard };
        if best.as_ref().is_none_or(|b| (cand.has_hard, cand.pick) < (b.has_hard, b.pick)) {
            best = Some(cand);
        }
        if !hard {
            passed += 1;
            if passed >= compare {
                break;
            }
        }
    }

    let best = match best {
        Some(b) => b,
        None => {
            if gdir.join(format!("chunk_{ci:03}.wav")).exists() {
                return Err(RenderError::Value("Could not safely separate the sentence ending from its continuation. Try Re-speak again; the previous take was kept.".into()));
            }
            // a rare inseparable pause must not stop a long narration: a new plain take, flagged for review
            r.check()?;
            seed = next_free(seed.wrapping_add(1), &used);
            used.insert(seed);
            let per = text::word_count(&ch_text) as f64 / assets.wps;
            let expected_seconds = (if 1.0 >= per { 1.0 } else { per }) * budget;
            let t0 = (r.clock)();
            let take = r.voice.synthesize(&engine, &ch_text, &assets.ref_wav, &assets.ref_text, &language, &p, seed, expected_seconds, r.cancel)?;
            r.check()?;
            let mut ev = r.evaluate(&take.audio, take.sr, &ch_text, &assets, &s, &language, nmax, None)?;
            ev.issues.push("ending_unverified");
            ev.score += 2.5;
            if take.report.get("stop_reason") == Some(&Value::from("length_limit_reached")) {
                if !ev.issues.contains(&"cut_off") {
                    ev.issues.push("cut_off");
                }
                budget = { let x = budget * 1.3; if 4.0 <= x { 4.0 } else { x } };
            }
            let secs = py_round((r.clock)() - t0, 1);
            attempts.push(json!({"seed": seed, "issues": issues_value(&ev.issues), "score": ev.score, "wer": ev.wer,
                "sim": ev.sim, "dur": ev.dur, "max_gap": ev.max_gap, "heard": ev.details.as_ref().map_or("", |d| d.heard.as_str()),
                "synthesis": take.report, "expected_seconds": expected_seconds, "ending_guard_fallback": true,
                "temperature": p.get("temperature").cloned().unwrap_or(Value::Null), "secs": secs}));
            Cand { ev, context: None, full_raw: None, seed, pitch_hz: None, pick: 0.0, has_hard: true }
        }
    };
    let sr = best.ev.sr;
    let mut hard: Vec<&str> = best.ev.issues.iter().copied().filter(|i| is_hard(i)).collect();
    let d = best.ev.details.as_ref();
    // the kept take's word times inside the saved part
    let mut words: Option<Vec<PartWord>> = None;
    if let Some(d) = d.filter(|d| !d.words.is_empty()) {
        if let (Some(trim), Some(pad)) = (d.ending.trim_start, d.ending.head_pad_s) {
            let shift = pad - trim; // Whisper ran on the untrimmed take
            words = Some(d.words.iter().map(|w| PartWord { word: w.word.clone(), start: w.start + shift, end: w.end + shift }).collect());
        }
    }
    let mut a = best.ev.audio.clone();
    let inner = py::int(s.get("inner_sentence_pause_ms").unwrap_or(&Value::from(0)))?;
    if hard.contains(&"long_silence") {
        // repair: collapse any over-long internal pause in the chosen take
        a = audio::shorten_gaps(&a, sr, d.map_or(&[][..], |d| &d.gaps), 0.45);
        hard.retain(|h| *h != "long_silence");
        words = None; // shortcut: the cut moves later words, so this rare part gets estimated timings
    } else if inner != 0 && words.as_ref().is_some_and(|w| !w.is_empty()) {
        let (x, w) = widen_sentence_pauses(&a, sr, &ch_text, words.as_ref().unwrap(), inner as f64 / 1000.0);
        a = x;
        words = Some(w);
    }
    let a = audio::normalize_lufs(&a, sr, py::num(get(&s, "target_lufs")?)?, -1.0);
    let fname = format!("chunk_{ci:03}.wav");
    audio::save(&gdir.join(&fname), &a, sr).map_err(|e| format!("{}: {e}", gdir.join(&fname).display()))?;
    let raw_fname = format!("raw_chunk_{ci:03}.wav");
    files::write_float_wav(&gdir.join(&raw_fname), d.map_or(&best.ev.audio, |d| &d.raw_audio), sr)?;
    let context_fname = format!("context_chunk_{ci:03}.wav");
    let has_context = best.context.as_ref().is_some_and(|c| !c.is_empty());
    if has_context {
        files::write_float_wav(&gdir.join(&context_fname), best.full_raw.as_deref().unwrap_or(&[]), sr)?;
    } else if let Err(e) = std::fs::remove_file(gdir.join(&context_fname)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(RenderError::Other(format!("{}: {e}", gdir.join(&context_fname).display())));
        }
    }
    let rev = ch.get("audio_revision").map_or(Ok(0), py::int)? + 1;
    let words_v = match words.filter(|w| !w.is_empty()) {
        Some(w) => Value::Array(w.iter().map(|w| json!({"word": w.word, "start": py_round(w.start, 3), "end": py_round(w.end, 3)})).collect()),
        None => Value::Null,
    };
    let mut out = ch.clone();
    for (k, v) in [
        ("status", Value::from(if hard.is_empty() { "ok" } else { "flagged" })),
        ("file", fname.into()),
        ("sr", sr.into()),
        ("attempts", Value::Array(attempts)),
        ("wer", f(best.ev.wer)),
        ("sim", f(best.ev.sim)),
        ("dur", f(py_round(a.len() as f64 / sr as f64, 2))),
        ("issues", issues_value(&hard)),
        ("heard", d.map_or("", |d| d.heard.as_str()).into()),
        ("raw_file", raw_fname.into()),
        ("ending", d.map_or(Value::Null, |d| ending_doc(&d.ending))),
        ("audio_revision", rev.into()),
        ("used_seeds", Value::Array(used.iter().map(|&x| x.into()).collect())),
        ("duration_budget", f(budget)),
        ("pitch_hz", opt(best.pitch_hz)),
        ("words", words_v),
        ("ending_guard", best.context.clone().map_or(Value::Null, Value::Object)),
        ("context_file", if has_context { context_fname.into() } else { Value::Null }),
        ("chosen_seed", best.seed.into()),
        ("accepted", Value::Null), // "Sounds fine" belongs to the take a person heard
    ] {
        out.insert(k.into(), v);
    }
    if let Some(Value::Array(list)) = narration.get_mut("chunks") {
        list[ci] = Value::Object(out.clone());
    }
    Ok(out)
}
