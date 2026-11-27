//! A whole narration (step S16): backend/app/pipeline.py `assemble`, `apply_speed`, `retime`, `_save`,
//! `run_generation`, `final_scan`, `_snapshot`/`_restore`/`_discard`, `regenerate_chunk`, `select_take`,
//! `fix_remaining`, and delivery.py `backfill`.
//! The library, the clock (`time.time`), the phase hook (worker.py `phase`) and, for the steps that record,
//! the models, the cancel flag, the nonce (`secrets.randbits(32)`) and the model load are inputs, so tests run the
//! scripted fake. `assemble`, `apply_speed`, `retime`, `final_scan` and `select_take` need no model.

use super::{phased, render_chunk, Phase, Render, RenderError};
use super::models::{SpeakerEncoder, Transcriber, Voice};
use crate::audio::{self, ffmpeg, np};
use crate::checks::py_round;
use crate::delivery;
use crate::library::{self, Doc, Library};
use crate::mastering;
use crate::pipeline::{self, processed_edge_issues, Fail};
use crate::review::{self, TAKE_FIELDS};
use crate::text::{fix_entities, py};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// What every step reaches: the library, `time.time` and the job's phase hook.
pub struct Ctx<'a> {
    pub lib: &'a Library,
    pub clock: &'a mut dyn FnMut() -> f64,
    pub phase: Phase<'a>,
}

/// What a step that records takes reaches besides `Ctx`. `load` is `engines.load(engine)` (an error names the
/// missing model file; there is no download); `nonce` is `secrets.randbits(32)`.
pub struct Models<'a> {
    pub voice: &'a mut dyn Voice,
    pub transcriber: &'a mut dyn Transcriber,
    pub encoder: &'a mut dyn SpeakerEncoder,
    pub cancel: &'a AtomicBool,
    pub nonce: &'a mut dyn FnMut() -> u32,
    pub load: &'a mut dyn FnMut() -> Result<(), String>,
    pub pitch_cache: &'a mut HashMap<String, f64>,
}

/// pipeline.py's message when a render is cancelled.
pub const STOPPED: &str = "Stopped. Finished parts are kept; press Resume to finish the rest.";
const RESPEAK_FAILED: &str = "The re-speak did not finish. The previous text and audio were kept.";
const RESPEAK_STOPPED: &str = "The re-speak was stopped. The previous text and audio were kept.";

fn other(e: impl std::fmt::Display) -> RenderError {
    RenderError::Other(e.to_string())
}

fn fail(e: Fail) -> RenderError {
    match e {
        Fail::Value(m) => RenderError::Value(m),
        Fail::Other(m) => RenderError::Other(m),
    }
}

fn get<'a>(d: &'a Doc, k: &str) -> Result<&'a Value, RenderError> {
    d.get(k).ok_or_else(|| RenderError::Other(format!("KeyError: '{k}'")))
}

/// `float(d.get(k) or 1.0)`.
fn speed_of(d: &Doc) -> Result<f64, RenderError> {
    match d.get("speed").filter(|v| py::truthy(v)) {
        Some(v) => Ok(py::num(v)?),
        None => Ok(1.0),
    }
}

fn chunks_mut(doc: &mut Doc) -> Result<&mut Vec<Value>, RenderError> {
    doc.get_mut("chunks").and_then(Value::as_array_mut).ok_or_else(|| other("KeyError: 'chunks'"))
}

/// `gen["chunks"][ci]` (a position, as Python indexes it).
fn part(doc: &mut Doc, ci: i64) -> Result<&mut Doc, RenderError> {
    let list = chunks_mut(doc)?;
    let n = list.len() as i64;
    let i = if ci < 0 { n + ci } else { ci };
    list.get_mut(i as usize).filter(|_| (0..n).contains(&i)).and_then(Value::as_object_mut).ok_or_else(|| other("IndexError: list index out of range"))
}

fn remove_if_there(p: &Path) -> Result<(), RenderError> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(other(format!("{}: {e}", p.display()))),
        _ => Ok(()),
    }
}

fn replace(from: &Path, to: &Path) -> Result<(), RenderError> {
    std::fs::rename(from, to).map_err(|e| other(format!("{} -> {}: {e}", from.display(), to.display())))
}

/// `{**db.get_settings(), **gen["settings"]}`.
fn settings_for(lib: &Library, doc: &Doc) -> Result<Doc, RenderError> {
    let mut s = library::get_settings(&lib.settings_path())?;
    if let Some(Value::Object(g)) = doc.get("settings") {
        s.extend(g.clone());
    }
    Ok(s)
}

// ------------------------------------------------------------------ delivery.backfill

/// `json.dumps({"sr": sr, "bands": bands})`: Python writes NaN for the bands above 0.92 x Nyquist, which
/// `delivery::read_mic_bands` reads back.
fn mic_json(sr: u32, bands: &[f64]) -> String {
    let b: Vec<String> = bands
        .iter()
        .map(|x| match *x {
            x if x.is_nan() => "NaN".into(),
            x if x == f64::INFINITY => "Infinity".into(),
            x if x == f64::NEG_INFINITY => "-Infinity".into(),
            // shortcut: serde's shortest float text; Python writes 1e-05 where serde writes 1e-5 (bands are dB to 0.1)
            x => Value::from(x).to_string(),
        })
        .collect();
    format!("{{\"sr\": {sr}, \"bands\": [{}]}}", b.join(", "))
}

/// `delivery.backfill(profile, pdir)`: tags, pitch level and mic profile for voices analyzed before those
/// features, without changing the chosen clip or transcript edits. True when the profile changed.
pub fn backfill(profile: &mut Doc, pdir: &Path) -> Result<bool, String> {
    let mut changed = false;
    let needs_tags = match profile.get("variants") {
        Some(Value::Array(vs)) if !vs.is_empty() => vs[0].as_object().is_none_or(|v| !v.contains_key("expressiveness")),
        _ => false,
    };
    if needs_tags {
        let vs = profile.get_mut("variants").and_then(Value::as_array_mut).expect("checked above");
        let mut raw = Vec::new();
        for v in vs.iter_mut() {
            let v = v.as_object_mut().ok_or("a variant is not an object")?;
            let file = py::display(v.get("file").ok_or("KeyError: 'file'")?);
            let (a, sr) = audio::load(&pdir.join(file), None)?;
            let pro = delivery::prosody(&a, sr);
            v.insert("pitch_range".into(), py_round(pro.pitch_range, 1).into());
            raw.push(delivery::expressiveness(&pro));
        }
        // argsort().argsort(): each value's rank. shortcut: a stable sort; numpy's quicksort may order exact ties
        // differently on more than 16 clips (a voice has a handful)
        let mut order: Vec<usize> = (0..raw.len()).collect();
        order.sort_by(|&i, &j| raw[i].partial_cmp(&raw[j]).unwrap_or(std::cmp::Ordering::Equal));
        let mut rank = vec![0usize; raw.len()];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r;
        }
        let n = vs.len();
        let denom = (n.saturating_sub(1)).max(1) as f64;
        for (v, r) in vs.iter_mut().zip(rank) {
            let r = r as f64 / denom;
            let v = v.as_object_mut().expect("checked above");
            v.insert("expressiveness".into(), if n > 1 { py_round(r, 3) } else { 0.5 }.into());
            v.insert("delivery".into(), if r >= 0.67 { "expressive" } else if r <= 0.33 { "calm" } else { "balanced" }.into());
        }
        changed = true;
    }
    if !profile.get("pitch_hz").is_some_and(py::truthy) && pdir.join("clean.wav").exists() {
        let (a, sr) = audio::load(&pdir.join("clean.wav"), None)?; // the whole cleaned sample: the voice's own pitch level
        let hz = py_round(delivery::prosody(&a, sr).pitch_hz, 1);
        profile.insert("pitch_hz".into(), if hz != 0.0 { hz.into() } else { Value::Null });
        changed = changed || hz != 0.0;
    }
    let mic = pdir.join("mic.json");
    if !mic.exists() {
        // `pdir / profile.get("original_file", "")`: with no name this is the folder itself, as in Python
        let src = pdir.join(profile.get("original_file").map(py::display).unwrap_or_default());
        if src.exists() {
            let tmp = pdir.join("_mic_tmp.wav");
            ffmpeg::decode_to_wav(&src, &tmp, 24000, Some(300.0))?;
            let loaded = audio::load(&tmp, None);
            let _ = std::fs::remove_file(&tmp);
            let (a, sr) = loaded?;
            std::fs::write(&mic, mic_json(sr, &delivery::ltas(&a, sr))).map_err(|e| format!("{}: {e}", mic.display()))?;
            changed = true;
        }
    }
    Ok(changed)
}

// ------------------------------------------------------------------ assembly and speed

/// `apply_speed(gdir, speed, master)`: pitch-preserving tempo change from the 1.0x master. WAV and MP3 are both
/// encoded first and only then published, so a failed encode leaves the previous pair intact.
pub fn apply_speed(gdir: &Path, speed: f64, master: Option<&Path>, phase: Phase) -> Result<(), RenderError> {
    phased(phase, "speed_export", || {
        let master = master.map_or_else(|| gdir.join("master.wav"), Path::to_path_buf);
        let (tmp_wav, tmp_mp3) = (gdir.join("final.tmp.wav"), gdir.join("final.tmp.mp3"));
        let made = (|| -> Result<(), RenderError> {
            if (speed - 1.0).abs() < 1e-3 {
                std::fs::copy(&master, &tmp_wav).map_err(|e| other(format!("{}: {e}", master.display())))?;
            } else {
                ffmpeg::atempo(&master, &tmp_wav, speed)?;
            }
            ffmpeg::export_mp3(&tmp_wav, &tmp_mp3)?;
            replace(&tmp_wav, &gdir.join("final.wav"))?;
            replace(&tmp_mp3, &gdir.join("final.mp3"))
        })();
        let _ = std::fs::remove_file(&tmp_wav);
        let _ = std::fs::remove_file(&tmp_mp3);
        made
    })
}

/// `retime(gen, speed)`: a new playback speed for a finished narration without re-rendering.
pub fn retime(generations_dir: &Path, doc: &mut Doc, speed: f64, clock: &mut dyn FnMut() -> f64, phase: Phase) -> Result<(), RenderError> {
    let gdir = generations_dir.join(py::display(get(doc, "id")?));
    let old = speed_of(doc)?;
    if !gdir.join("master.wav").exists() {
        return Err(RenderError::Value("Re-render this generation to change its speed.".into()));
    }
    apply_speed(&gdir, speed, None, phase)?;
    let k = old / speed;
    let mut tl = Vec::new();
    for x in doc.get("timeline").and_then(Value::as_array).into_iter().flatten() {
        let mut x = x.as_object().ok_or_else(|| other("a timeline entry is not an object"))?.clone();
        let (s0, e0) = (py::num(get(&x, "start")?)?, py::num(get(&x, "end")?)?);
        x.insert("start".into(), py_round(s0 * k, 3).into());
        x.insert("end".into(), py_round(e0 * k, 3).into());
        tl.push(Value::Object(x));
    }
    doc.insert("timeline".into(), tl.into());
    let d = match doc.get("duration").filter(|v| py::truthy(v)) {
        Some(v) => py::num(v)?,
        None => 0.0,
    };
    doc.insert("duration".into(), py_round(d * k, 2).into());
    doc.insert("speed".into(), speed.into());
    doc.insert("audio_rev".into(), clock().into());
    Ok(())
}

/// `assemble(gen, s)`: the kept parts joined with the pause settings, mic match and studio finish when on,
/// loudness, the 1.0x `master.wav` and the speed-changed `final.wav` / `final.mp3`; the timeline for subtitles.
pub fn assemble(cx: &mut Ctx, doc: &mut Doc, s: &Doc) -> Result<(), RenderError> {
    let ph = cx.phase;
    phased(ph, "assembly", || assemble_now(cx, doc, s))
}

fn assemble_now(cx: &mut Ctx, doc: &mut Doc, s: &Doc) -> Result<(), RenderError> {
    let gdir = cx.lib.generations_dir().join(py::display(get(doc, "id")?));
    let (mut parts, mut pauses, mut idx, mut sr_out): (Vec<Vec<f32>>, Vec<f64>, Vec<Value>, Option<u32>) = Default::default();
    for ch in doc.get("chunks").and_then(Value::as_array).ok_or_else(|| other("KeyError: 'chunks'"))? {
        let ch = ch.as_object().ok_or_else(|| other("a part is not an object"))?;
        let Some(file) = ch.get("file").filter(|f| py::truthy(f)) else { continue };
        let (a, sr) = audio::load(&gdir.join(py::display(file)), None)?;
        let so = *sr_out.get_or_insert(sr);
        parts.push(audio::resample(&a, sr, so));
        idx.push(get(ch, "i")?.clone());
        let pause = match get(ch, "pause_after")?.as_str() {
            Some("sentence") => py::num(get(s, "sentence_pause_ms")?)?,
            Some("paragraph") => py::num(get(s, "paragraph_pause_ms")?)?,
            Some("end") => 0.0,
            other_key => return Err(other(format!("KeyError: {other_key:?}"))),
        };
        pauses.push(pause);
    }
    let Some(sr_out) = sr_out else { return Ok(()) };
    let lead = 0.15;
    let n = parts.len();
    // Each part already has a little natural room at both edges; add only what's missing so the audible gap
    // between sentences equals the setting.
    let edges: Vec<(f64, f64)> = parts.iter().map(|p| audio::edge_silence(p, sr_out)).collect();
    let pauses: Vec<i64> = (0..n)
        .map(|i| if i == n - 1 { 0 } else { ((pauses[i] - 1000.0 * (edges[i].1 + edges[i + 1].0)).trunc() as i64).max(0) })
        .collect();
    // timeline of each part inside the final file (for subtitles)
    let mut timeline = Vec::new();
    let mut t = lead;
    for (i, p) in parts.iter().enumerate() {
        let d = p.len() as f64 / sr_out as f64;
        timeline.push((idx[i].clone(), py_round(t, 3), py_round(t + d, 3)));
        t += d + pauses[i] as f64 / 1000.0;
    }
    let refs: Vec<&[f32]> = parts.iter().map(Vec::as_slice).collect();
    let joined = audio::concat_with_pauses(&refs, &pauses, sr_out);
    let mut full = vec![0.0f32; (lead * sr_out as f64) as usize];
    full.extend(joined);
    full.extend(std::iter::repeat_n(0.0f32, (0.4 * sr_out as f64) as usize));
    let mut vs = delivery::default_voice_settings();
    if let Some(Value::Object(v)) = doc.get("voice_settings") {
        vs.extend(v.clone());
    }
    let mic = cx.lib.profiles_dir().join(py::display(get(doc, "profile_id")?)).join("mic.json");
    doc.insert("mic_eq".into(), Value::Null);
    if vs.get("mic_match").is_some_and(py::truthy) && mic.exists() {
        let (out, curve) = delivery::mic_match(&full, sr_out, &delivery::read_mic_bands(&mic)?, 6.0);
        full = out;
        doc.insert("mic_eq".into(), if curve.is_empty() { Value::Null } else { curve.into() });
    }
    if vs.get("studio_finish").is_some_and(py::truthy) {
        full = mastering::master_chain(&full, sr_out).map_err(|e| other(format!("{}: {}", e.kind, e.message)))?;
    }
    let full = audio::normalize_lufs(&full, sr_out, py::num(get(s, "target_lufs")?)?, -1.0);
    let speed = speed_of(doc)?;
    let tmp_master = gdir.join("master.tmp.wav");
    audio::save(&tmp_master, &full, sr_out).map_err(|e| other(format!("{}: {e}", tmp_master.display())))?;
    if let Err(e) = apply_speed(&gdir, speed, Some(&tmp_master), cx.phase) {
        let _ = std::fs::remove_file(&tmp_master);
        return Err(e);
    }
    replace(&tmp_master, &gdir.join("master.wav"))?; // 1.0x master, kept for re-timing
    doc.insert("audio_rev".into(), (cx.clock)().into());
    let tl: Vec<Value> = timeline
        .into_iter()
        .map(|(i, a, b)| json!({"i": i, "start": py_round(a / speed, 3), "end": py_round(b / speed, 3)}))
        .collect();
    doc.insert("timeline".into(), tl.into());
    doc.insert("file".into(), "final.wav".into());
    doc.insert("duration".into(), py_round(full.len() as f64 / sr_out as f64 / speed, 2).into());
    doc.insert("sample_rate".into(), sr_out.into());
    Ok(())
}

// ------------------------------------------------------------------ final check

/// `final_scan(gen)`: raw endings and the speech kept at each processed part's edges. Writes each timed part's
/// `final_issues` and returns the parts that fail.
pub fn final_scan(lib: &Library, doc: &mut Doc, phase: Phase) -> Result<Vec<i64>, RenderError> {
    phased(phase, "final_check", || final_scan_now(lib, doc))
}

fn final_scan_now(lib: &Library, doc: &mut Doc) -> Result<Vec<i64>, RenderError> {
    let gdir = lib.generations_dir().join(py::display(get(doc, "id")?));
    let tl: Vec<Value> = doc.get("timeline").and_then(Value::as_array).cloned().unwrap_or_default();
    let final_path = gdir.join(doc.get("file").filter(|f| py::truthy(f)).map_or_else(|| "final.wav".to_string(), py::display));
    let fin = if final_path.exists() { Some(audio::load(&final_path, None)?) } else { None };
    let speed = speed_of(doc)?;
    let mut by_i: HashMap<i64, usize> = HashMap::new();
    for (pos, c) in chunks_mut(doc)?.iter().enumerate() {
        by_i.insert(py::int(c.get("i").ok_or_else(|| other("KeyError: 'i'"))?)?, pos);
    }
    let mut bad = Vec::new();
    for t in &tl {
        let t = t.as_object().ok_or_else(|| other("a timeline entry is not an object"))?;
        let ti = py::int(get(t, "i")?)?;
        let pos = *by_i.get(&ti).ok_or_else(|| other(format!("KeyError: {ti}")))?;
        let ch = chunks_mut(doc)?[pos].as_object_mut().ok_or_else(|| other("a part is not an object"))?;
        let mut issues: BTreeSet<String> = BTreeSet::new();
        let ending = ch.get("ending").and_then(Value::as_object).cloned().unwrap_or_default();
        if let Some(raw_file) = ch.get("raw_file").filter(|f| py::truthy(f)) {
            let raw_path = gdir.join(py::display(raw_file));
            if raw_path.exists() {
                let (raw, raw_sr) = audio::load(&raw_path, None)?;
                let whole = raw.len() as f64 / raw_sr as f64;
                let last_word_end = ending.get("last_word_end").map_or(Ok(whole), py::num)?;
                let e = audio::end_analysis(&raw, raw_sr, last_word_end, 0.12);
                if !e.natural {
                    issues.insert("cut_off".into());
                }
                if e.junk {
                    issues.insert("junk_tail".into());
                }
                if audio::lead_burst(&raw, raw_sr).is_some() {
                    issues.insert("lead_burst".into());
                }
                // A damaged part can match its export perfectly: also inspect sound its original trim discarded.
                let saved_cut = ending.get("cut").map_or(Ok(whole), py::num)?;
                let lo = ((saved_cut * raw_sr as f64).round_ties_even() as i64).max(0);
                let hi = (e.cut * raw_sr as f64).round_ties_even() as i64;
                let discarded = &raw[np::py_slice(raw.len(), lo, hi)];
                if discarded.len() as f64 >= raw_sr as f64 * 0.02 {
                    let speech_level = np::percentile_f32(&audio::frame_db(&raw, raw_sr, 10), 90.0).map_err(|e| other(e.message))? as f64;
                    let line = (speech_level - 48.0) as f32; // numpy 2 compares float32 frames with the weak Python float in float32
                    if audio::frame_db(discarded, raw_sr, 10).iter().filter(|&&d| d > line).count() >= 2 {
                        issues.insert("processed_audio_cut_off".into());
                    }
                }
            }
        }
        // Known cut-off takes never become clean because a fade in the stored WAV hides the hard stop.
        for i in ch.get("issues").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            if i == "cut_off" || i == "junk_tail" {
                issues.insert(i.into());
            }
        }
        // The processed file against each saved part, including the silence assembly padded.
        if let Some(file) = ch.get("file").filter(|f| py::truthy(f)) {
            let chunk_path = gdir.join(py::display(file));
            if chunk_path.exists() {
                match &fin {
                    None => {
                        issues.insert("processed_audio_missing".into());
                    }
                    Some((fa, fsr)) => {
                        let (chunk, _) = audio::load(&chunk_path, Some(*fsr))?;
                        let n = ((chunk.len() as f64 / speed).round_ties_even() as usize).max(1);
                        // approximate atempo's energy alignment without another ffmpeg call
                        let x: Vec<f64> = (0..n).map(|k| k as f64 * speed).collect();
                        let xp: Vec<f64> = (0..chunk.len()).map(|k| k as f64).collect();
                        let fp: Vec<f64> = chunk.iter().map(|&v| v as f64).collect();
                        if xp.is_empty() {
                            return Err(RenderError::Value("array of sample points is empty".into())); // np.interp's error
                        }
                        let expected: Vec<f32> = np::interp(&x, &xp, &fp).into_iter().map(|v| v as f32).collect();
                        let start = ((py::num(get(t, "start")?)? * *fsr as f64).round_ties_even() as i64).max(0);
                        let segment = &fa[np::py_slice(fa.len(), start, start + n as i64)];
                        issues.extend(processed_edge_issues(&expected, segment, *fsr).into_iter().map(String::from));
                    }
                }
            }
        }
        let has = !issues.is_empty();
        ch.insert("final_issues".into(), issues.into_iter().collect::<Vec<_>>().into());
        if has {
            bad.push(ti);
        }
    }
    Ok(bad)
}

// ------------------------------------------------------------------ a whole render

/// The spec name `run_generation` shows while loading (engines.py `BUILTIN_ENGINES`).
fn spec_name(engine: &str) -> Result<&'static str, RenderError> {
    match engine {
        "qwen3-tts-1.7b" => Ok("Qwen3-TTS 1.7B"),
        "qwen3-tts-0.6b" => Ok("Qwen3-TTS 0.6B (fast)"),
        _ => Err(RenderError::Other(format!("Engine {engine} is not in TakeTwo 1.0: only Qwen3-TTS renders."))),
    }
}

/// `_save(gen)`: persist worker progress; a narration deleted meanwhile stops the run.
fn save(lib: &Library, doc: &mut Doc) -> Result<(), RenderError> {
    if lib.put_generation_live(doc)? { Ok(()) } else { Err(RenderError::Cancelled) }
}

fn check(cancel: &AtomicBool) -> Result<(), RenderError> {
    if cancel.load(Ordering::SeqCst) { Err(RenderError::Cancelled) } else { Ok(()) }
}

/// One `render_chunk` call with the library's `put_profile`.
#[allow(clippy::too_many_arguments)]
fn record(cx: &mut Ctx, m: &mut Models, doc: &mut Doc, ci: i64, profile: &mut Doc, s: &Doc, regen_round: i64) -> Result<(), RenderError> {
    let lib = cx.lib;
    let (profiles, generations) = (lib.profiles_dir(), lib.generations_dir());
    let mut put = |p: &mut Doc| lib.put_profile(p);
    let nonce = (m.nonce)();
    let mut r = Render {
        profiles_dir: &profiles,
        generations_dir: &generations,
        voice: &mut *m.voice,
        transcriber: &mut *m.transcriber,
        encoder: &mut *m.encoder,
        put_profile: &mut put,
        clock: &mut *cx.clock,
        cancel: m.cancel,
        pitch_cache: &mut *m.pitch_cache,
        phase: cx.phase,
    };
    let ci = usize::try_from(ci).map_err(|_| other("IndexError: list index out of range"))?;
    render_chunk(&mut r, doc, ci, profile, s, regen_round, nonce).map(|_| ())
}

fn int_or_0(d: &Doc, k: &str) -> Result<i64, RenderError> {
    Ok(d.get(k).map_or(Ok(0), py::int)?)
}

/// `run_generation(gen_id, cancel, only_chunks)`: record the parts that are not `ok` (or only `only`), assemble,
/// final check (re-recording what it finds once), then the status, summary and timings. A stop is saved as a
/// status (`cancelled`), never returned as an error; a narration deleted meanwhile is never recreated.
pub fn run_generation(cx: &mut Ctx, m: &mut Models, gid: &str, only: Option<&[i64]>) -> Result<(), RenderError> {
    let lib = cx.lib;
    let mut doc = lib.get_generation(gid)?.ok_or_else(|| other(format!("No narration {gid}.")))?;
    let mut profile = lib.get_profile(&py::display(get(&doc, "profile_id")?))?.ok_or_else(|| RenderError::Value("Voice profile was deleted.".into()))?;
    let s = settings_for(lib, &doc)?;
    let pdir = lib.profiles_dir().join(py::display(get(&profile, "id")?));
    if backfill(&mut profile, &pdir)? {
        lib.put_profile(&mut profile)?;
    }
    let todo: Vec<i64> = match only {
        Some(o) => o.to_vec(),
        None => {
            let mut t = Vec::new();
            for c in doc.get("chunks").and_then(Value::as_array).ok_or_else(|| other("KeyError: 'chunks'"))? {
                if c.get("status").and_then(Value::as_str) != Some("ok") {
                    t.push(py::int(c.get("i").ok_or_else(|| other("KeyError: 'i'"))?)?);
                }
            }
            t
        }
    };
    doc.insert("status".into(), "running".into());
    let name = spec_name(&py::display(get(&doc, "engine")?))?;
    match run_body(cx, m, &mut doc, &mut profile, &s, &todo, only, name) {
        Ok(()) => {}
        Err(RenderError::Cancelled) => {
            doc.insert("status".into(), "cancelled".into());
            doc.insert("message".into(), STOPPED.into());
            for c in chunks_mut(&mut doc)? {
                if c.get("status").and_then(Value::as_str) == Some("running") {
                    c["status"] = "pending".into();
                }
            }
        }
        Err(e) => return Err(e),
    }
    lib.put_generation_live(&mut doc)?; // false when the narration was deleted: nothing is recreated
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_body(cx: &mut Ctx, m: &mut Models, doc: &mut Doc, profile: &mut Doc, s: &Doc, todo: &[i64], only: Option<&[i64]>, name: &str) -> Result<(), RenderError> {
    let lib = cx.lib;
    doc.insert("message".into(), format!("Loading {name}").into());
    save(lib, doc)?;
    (m.load)()?;
    let t0 = (cx.clock)();
    let n_parts = chunks_mut(doc)?.len();
    for (n, &ci) in todo.iter().enumerate() {
        doc.insert("message".into(), format!("Part {} of {n_parts}", ci + 1).into());
        part(doc, ci)?.insert("status".into(), "running".into());
        save(lib, doc)?;
        let round = int_or_0(part(doc, ci)?, "regen_count")?;
        record(cx, m, doc, ci, profile, s, round)?;
        doc.insert("progress".into(), py_round((n + 1) as f64 / todo.len() as f64, 4).into());
        let elapsed = (cx.clock)() - t0;
        doc.insert("eta".into(), ((elapsed / (n + 1) as f64 * (todo.len() - n - 1) as f64).round_ties_even() as i64).into());
        save(lib, doc)?;
    }
    doc.insert("message".into(), "Assembling".into());
    save(lib, doc)?;
    assemble(cx, doc, s)?;
    // Every render, including one-part replacements, receives a final check.
    doc.insert("message".into(), "Final check: checking the finished narration for audio defects".into());
    save(lib, doc)?;
    // A re-speak or fix records only its own parts; anything else the check finds gets flagged below.
    let bad: Vec<i64> = final_scan(lib, doc, cx.phase)?.into_iter().filter(|ci| only.is_none_or(|o| o.contains(ci))).collect();
    if !bad.is_empty() {
        for (n, &ci) in bad.iter().enumerate() {
            check(m.cancel)?;
            doc.insert("message".into(), format!("Final check: re-recording part {} ({} of {})", ci + 1, n + 1, bad.len()).into());
            let ch = part(doc, ci)?;
            let round = int_or_0(ch, "regen_count")? + 1;
            ch.insert("regen_count".into(), round.into());
            ch.insert("status".into(), "running".into());
            save(lib, doc)?;
            record(cx, m, doc, ci, profile, s, round)?;
        }
        check(m.cancel)?;
        assemble(cx, doc, s)?;
    }
    let remaining = final_scan(lib, doc, cx.phase)?;
    check(m.cancel)?; // a stop during the final take is honoured, never marked done
    // A part needs a listen when its take or its final check says so; recomputed every run.
    for c in chunks_mut(doc)? {
        let c = c.as_object_mut().ok_or_else(|| other("a part is not an object"))?;
        if matches!(c.get("status").and_then(Value::as_str), Some("ok" | "flagged")) {
            let needs = c.get("issues").is_some_and(py::truthy) || c.get("final_issues").is_some_and(py::truthy);
            c.insert("status".into(), if needs { "flagged" } else { "ok" }.into());
        }
    }
    let mut fixed = Vec::new();
    for &ci in &bad {
        if !remaining.contains(&ci) && part(doc, ci)?.get("status").and_then(Value::as_str) == Some("ok") {
            fixed.push(ci);
        }
    }
    for &ci in &bad {
        part(doc, ci)?.insert("final_fix".into(), fixed.contains(&ci).into());
    }
    let checked = doc.get("timeline").and_then(Value::as_array).ok_or_else(|| other("KeyError: 'timeline'"))?.len();
    doc.insert("final_scan".into(), json!({"checked": checked, "attempted": bad, "fixed": fixed, "remaining": remaining}));
    doc.insert("status".into(), "done".into());
    let retries: usize = chunks_mut(doc)?.iter().map(|c| c.get("attempts").and_then(Value::as_array).map_or(0, |a| a.len().saturating_sub(1))).sum();
    doc.insert("retries".into(), retries.into());
    review::summarize(doc)?;
    doc.insert("render_seconds".into(), py_round((cx.clock)() - t0, 1).into());
    Ok(())
}

// ------------------------------------------------------------------ re-speak, takes, fix

/// The files a re-speak may replace.
fn respeak_files(ci: i64) -> Vec<String> {
    vec![format!("chunk_{ci:03}.wav"), format!("raw_chunk_{ci:03}.wav"), format!("context_chunk_{ci:03}.wav"), "final.wav".into(), "final.mp3".into(), "master.wav".into()]
}

/// `_snapshot(gdir, names)`: a `.bak` copy of each file that exists, so a failed run can put them back.
pub fn snapshot(gdir: &Path, names: &[String]) -> Result<Vec<(String, Option<PathBuf>)>, RenderError> {
    let mut snap = Vec::new();
    for n in names {
        let src = gdir.join(n);
        if src.exists() {
            let bak = gdir.join(format!("{n}.bak"));
            review::copy2(&src, &bak)?;
            snap.push((n.clone(), Some(bak)));
        } else {
            snap.push((n.clone(), None));
        }
    }
    Ok(snap)
}

/// `_restore(gdir, snap)`.
pub fn restore(gdir: &Path, snap: &[(String, Option<PathBuf>)]) -> Result<(), RenderError> {
    for (n, bak) in snap {
        match bak {
            Some(b) => replace(b, &gdir.join(n))?,
            None => remove_if_there(&gdir.join(n))?,
        }
    }
    Ok(())
}

/// `_discard(snap)`.
pub fn discard(snap: &[(String, Option<PathBuf>)]) {
    for (_, bak) in snap {
        if let Some(b) = bak {
            let _ = std::fs::remove_file(b);
        }
    }
}

/// `regenerate_chunk(gen_id, ci, new_text, cancel)`: re-speak one part. If the run fails or is stopped, the
/// previous text, audio, subtitles and export are restored together.
pub fn regenerate_chunk(cx: &mut Ctx, m: &mut Models, gid: &str, ci: i64, new_text: Option<&str>) -> Result<(), RenderError> {
    let lib = cx.lib;
    let mut doc = lib.get_generation(gid)?.ok_or_else(|| other(format!("No narration {gid}.")))?;
    let gdir = lib.generations_dir().join(gid);
    let mut before = doc.clone();
    if let Some(t) = new_text.filter(|t| !py::strip(t).is_empty()) {
        let settings = library::get_settings(&lib.settings_path())?;
        let cleaned = pipeline::clean_replacement(t, &doc, &settings).map_err(fail)?;
        part(&mut doc, ci)?.insert("text".into(), cleaned.into());
    }
    let now = (cx.clock)();
    let prev = review::archive_take(&lib.generations_dir(), gid, part(&mut doc, ci)?, &|| now)?; // Take 1 stays playable next to Take 2
    let ch = part(&mut doc, ci)?;
    if let Some(p) = &prev {
        let mut takes = ch.get("takes").and_then(Value::as_array).cloned().unwrap_or_default();
        takes.push(Value::Object(p.clone()));
        let mut top = i64::MIN;
        for x in &takes {
            top = top.max(py::int(x.get("take").ok_or_else(|| other("KeyError: 'take'"))?)?);
        }
        ch.insert("takes".into(), takes.into());
        ch.insert("take".into(), (top + 1).into());
    }
    let rc = int_or_0(ch, "regen_count")? + 1;
    ch.insert("regen_count".into(), rc.into());
    ch.insert("status".into(), "pending".into());
    // before anything is marked running, so a failure here leaves no stuck state
    let snap = snapshot(&gdir, &respeak_files(ci))?;
    doc.insert("status".into(), "running".into());
    let outcome = lib.put_generation_live(&mut doc).map_err(RenderError::from).and_then(|_| {
        run_generation(cx, m, gid, Some(&[ci]))
    });
    let mut stopped = true;
    let result = match outcome {
        Err(e) => {
            let r = restore(&gdir, &snap);
            before.insert("message".into(), RESPEAK_FAILED.into());
            let saved = lib.put_generation_live(&mut before);
            r.and(saved.map(|_| ()).map_err(RenderError::from)).and(Err(e))
        }
        Ok(()) => {
            let now = lib.get_generation(gid)?;
            stopped = now.as_ref().and_then(|g| g.get("status")).and_then(Value::as_str) == Some("cancelled");
            if stopped {
                restore(&gdir, &snap)?;
                before.insert("message".into(), RESPEAK_STOPPED.into());
                lib.put_generation_live(&mut before)?;
            } else if let Some(mut done) = now {
                review::trim_takes(&lib.generations_dir(), gid, part(&mut done, ci)?)?;
                lib.put_generation_live(&mut done)?;
            }
            Ok(())
        }
    };
    discard(&snap);
    if stopped && let Some(p) = prev {
        let k = py::display(p.get("take").unwrap_or(&Value::Null));
        let _ = std::fs::remove_dir_all(review::take_dir(&lib.generations_dir(), gid, ci, &k));
    }
    result
}

/// `select_take(gen_id, ci, k)`: keep an earlier take of one part instead of the current one, then re-mix.
/// The current take is archived first, so the switch can be undone the same way.
pub fn select_take(cx: &mut Ctx, gid: &str, ci: i64, k: i64) -> Result<(), RenderError> {
    let lib = cx.lib;
    let mut doc = match lib.get_generation(gid)? {
        Some(g) if !g.get("deleted_at").is_some_and(py::truthy) => g,
        _ => return Err(RenderError::Value("This narration was deleted.".into())),
    };
    let ch = part(&mut doc, ci)?;
    let takes = ch.get("takes").and_then(Value::as_array).cloned().unwrap_or_default();
    let is_k = |x: &Value| x.get("take").and_then(Value::as_f64) == Some(k as f64);
    let Some(t) = takes.iter().find(|x| is_k(x)).and_then(Value::as_object).cloned() else {
        return Err(RenderError::Value("That take is no longer kept.".into()));
    };
    let (gdir, src) = (lib.generations_dir().join(gid), review::take_dir(&lib.generations_dir(), gid, ci, &k.to_string()));
    let now = (cx.clock)();
    let cur = review::archive_take(&lib.generations_dir(), gid, ch, &|| now)?;
    let truthy_name = |d: &Doc, f: &str| d.get(f).filter(|v| py::truthy(v)).map(py::display);
    if let (Some(cf), None) = (truthy_name(ch, "context_file"), truthy_name(&t, "context_file")) {
        remove_if_there(&gdir.join(cf))?;
    }
    for f in ["file", "raw_file", "context_file"] {
        if let Some(name) = truthy_name(&t, f)
            && src.join(&name).exists()
        {
            replace(&src.join(&name), &gdir.join(&name))?;
        }
    }
    let _ = std::fs::remove_dir_all(&src);
    for f in TAKE_FIELDS {
        ch.insert(f.into(), t.get(f).cloned().unwrap_or(Value::Null));
    }
    ch.insert("take".into(), k.into());
    ch.insert("accepted".into(), Value::Null); // a person decides again for the take they now keep
    let rev = int_or_0(ch, "audio_revision")? + 1;
    ch.insert("audio_revision".into(), rev.into());
    let mut kept: Vec<Value> = takes.into_iter().filter(|x| !is_k(x)).collect();
    kept.extend(cur.map(Value::Object));
    ch.insert("takes".into(), kept.into());
    let s = settings_for(lib, &doc)?;
    assemble(cx, &mut doc, &s)?;
    if doc.get("status").and_then(Value::as_str) == Some("done") {
        review::summarize(&mut doc)?;
    }
    lib.put_generation_live(&mut doc)?; // keeps a delete made while the switch ran; writes nothing if the row is gone
    Ok(())
}

/// `fix_remaining(gen_id, cancel)`: re-record every part that still needs attention (`fix_targets`), with web
/// artifacts corrected in the stored script first.
pub fn fix_remaining(cx: &mut Ctx, m: &mut Models, gid: &str) -> Result<(), RenderError> {
    let lib = cx.lib;
    let mut doc = lib.get_generation(gid)?.ok_or_else(|| other(format!("No narration {gid}.")))?;
    let targets = review::fix_targets(&doc)?;
    if targets.is_empty() {
        return Err(RenderError::Value("Nothing left to fix.".into()));
    }
    for k in ["text", "cleaned_text"] {
        let t = doc.get(k).filter(|v| py::truthy(v)).map(py::display).unwrap_or_default();
        doc.insert(k.into(), fix_entities(&t).into());
    }
    for &ci in &targets {
        let ch = part(&mut doc, ci)?;
        let t = fix_entities(&py::display(get(ch, "text")?));
        ch.insert("text".into(), t.into());
        let rc = int_or_0(ch, "regen_count")? + 1;
        ch.insert("regen_count".into(), rc.into());
        ch.insert("status".into(), "pending".into());
    }
    doc.insert("status".into(), "running".into());
    doc.insert("message".into(), format!("Fixing {} remaining part(s)", targets.len()).into());
    lib.put_generation_live(&mut doc)?;
    run_generation(cx, m, gid, Some(&targets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mic_json_is_python_text() {
        assert_eq!(mic_json(24000, &[-1.5, 2.0, f64::NAN]), r#"{"sr": 24000, "bands": [-1.5, 2.0, NaN]}"#);
    }
}
