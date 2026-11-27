//! Voice previews (step S17): backend/app/pipeline.py `PREVIEW_TEXT`, `_preview_key`, `make_preview`, `drop_preview`.
//! A short sample spoken by the clone itself, with the same checks as a narration's takes.

use super::{noise_limit, profile_assets, py_eq, Render, RenderError, QWEN_KEYS};
use crate::audio;
use crate::checks::{py_round, HARD_ISSUES};
use crate::delivery;
use crate::library::{self, Doc, Library};
use crate::mastering;
use crate::text::{self, py};
use crate::render::narration;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::Ordering;

/// `PREVIEW_TEXT["en"]`, the only language Python has a preview text for.
pub const PREVIEW_TEXT_EN: &str = "Hi, this is my voice. Give me any script, and I'll read it clearly and naturally, just like this.";

/// `make_preview`'s three answers: True, False, "stale".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Saved,
    Nothing,
    Stale,
}

/// `_preview_key(profile)`: what a preview was made from (variant, its reference text, engine, language).
pub fn preview_key(profiles_dir: &Path, profile: &Doc) -> Result<(Value, String, Value, Value), String> {
    let vidx = profile.get("active_variant").cloned().unwrap_or_else(|| 0.into());
    let assets = profile_assets(profiles_dir, profile, Some(&vidx))?;
    Ok((vidx, assets.ref_text, profile.get("default_engine").cloned().unwrap_or(Value::Null), profile.get("language").cloned().unwrap_or(Value::Null)))
}

fn same_key(a: &(Value, String, Value, Value), b: &(Value, String, Value, Value)) -> bool {
    py_eq(&a.0, &b.0) && a.1 == b.1 && a.2 == b.2 && a.3 == b.3
}

/// `drop_preview(profile)`: the preview file and the document's `preview` go.
pub fn drop_preview(profiles_dir: &Path, profile: &mut Doc) -> Result<(), String> {
    let id = py::display(profile.get("id").ok_or("KeyError: 'id'")?);
    match std::fs::remove_file(profiles_dir.join(id).join("preview.wav")) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.to_string()),
        _ => {}
    }
    profile.remove("preview");
    Ok(())
}

/// One take's check result, with `has_hard` (Python adds it to the dict).
struct Ev {
    audio: Vec<f32>,
    sr: u32,
    issues: Vec<&'static str>,
    score: f64,
    has_hard: bool,
}

/// Python's `(has_hard, score) < (best_has_hard, best_score)`: a clean take beats a failed one whatever the scores,
/// then the lower score wins.
fn beats(new: (bool, f64), best: (bool, f64)) -> bool {
    new < best
}

/// `make_preview(pid, cancel)`. `downloaded` is `engines.is_downloaded(spec.repo)` (the Qwen model folder is there);
/// without it nothing is touched. Writes go through `r.put_profile`; `lib` reads the voice and the settings.
pub fn make_preview(r: &mut Render, lib: &Library, pid: &str, downloaded: bool) -> Result<Preview, RenderError> {
    let Some(mut profile) = lib.get_profile(pid)? else { return Ok(Preview::Nothing) };
    if profile.get("status").and_then(Value::as_str) != Some("ready") || !profile.get("variants").is_some_and(py::truthy) {
        return Ok(Preview::Nothing);
    }
    let language = profile.get("language").filter(|l| py::truthy(l)).map(py::display).unwrap_or_else(|| "en".into());
    let text = (language == "en").then_some(PREVIEW_TEXT_EN);
    let s = library::get_settings(&lib.settings_path())?;
    let engine = profile.get("default_engine").filter(|e| py::truthy(e)).or(s.get("default_engine")).map(py::display).unwrap_or_default();
    // shortcut: only the qwen3 family is in TakeTwo 1.0; another engine gets no preview (Python would load it)
    let qwen = matches!(engine.as_str(), "qwen3-tts-1.7b" | "qwen3-tts-0.6b");
    let Some(text) = text.filter(|_| downloaded && qwen) else { return Ok(Preview::Nothing) };
    let pdir = r.profiles_dir.join(pid);
    if narration::backfill(&mut profile, &pdir)? {
        (r.put_profile)(&mut profile)?;
    }
    let (params, qc) = delivery::resolve(Some(&Doc::new()), &QWEN_KEYS, Some(&Doc::new()), &s)?;
    let mut s = s;
    s.extend(qc);
    let key = preview_key(r.profiles_dir, &profile)?;
    let assets = profile_assets(r.profiles_dir, &profile, Some(&key.0))?;
    let nmax = noise_limit(r.profiles_dir, &mut profile, &mut *r.put_profile)?;
    let words = text::word_count(text) as f64;
    let mut best: Option<Ev> = None;
    let mut budget = 1.0f64;
    for k in 0..3u32 {
        if r.cancel.load(Ordering::SeqCst) {
            return Ok(Preview::Nothing);
        }
        let speak = if best.as_ref().is_some_and(|b| b.issues.contains(&"cut_off")) { format!("{text} —") } else { text.to_string() };
        let expected = (words / assets.wps).max(1.0) * budget;
        let take = r.synth(&engine, &speak, &assets, &language, &params, 4242 + k, expected)?;
        let e = r.evaluate(&take.audio, take.sr, text, &assets, &s, &language, nmax, None)?;
        let mut ev = Ev { audio: e.audio, sr: e.sr, issues: e.issues, score: e.score, has_hard: false };
        if take.report.get("stop_reason").and_then(Value::as_str) == Some("length_limit_reached") {
            budget = (budget * 1.3).min(4.0);
            if !ev.issues.contains(&"cut_off") {
                ev.issues.push("cut_off");
                ev.score += 2.5;
            }
        }
        // clean takes first, then the best score (a failed take must never beat a clean one)
        ev.has_hard = ev.issues.iter().any(|i| HARD_ISSUES.contains(i));
        let clean = !ev.has_hard;
        if best.as_ref().is_none_or(|b| beats((ev.has_hard, ev.score), (b.has_hard, b.score))) {
            best = Some(ev);
        }
        if clean {
            break;
        }
    }
    let best = best.expect("three takes ran");
    if best.issues.contains(&"cut_off") {
        return Ok(Preview::Nothing); // keep the existing preview rather than publish an incomplete take
    }
    let (mut a, sr) = (best.audio, best.sr);
    let mic = pdir.join("mic.json");
    if mic.exists() {
        a = delivery::mic_match(&a, sr, &delivery::read_mic_bands(&mic)?, 6.0).0;
    }
    a = mastering::master_chain(&a, sr).map_err(|e| RenderError::Other(format!("{}: {}", e.kind, e.message)))?;
    a = audio::normalize_lufs(&a, sr, -16.0, -1.0);
    let mut padded = vec![0.0f32; (0.1 * sr as f64) as usize];
    padded.extend_from_slice(&a);
    padded.extend(std::iter::repeat_n(0.0f32, (0.3 * sr as f64) as usize));
    let tmp = pdir.join("preview.tmp.wav");
    audio::save(&tmp, &padded, sr).map_err(|e| format!("{}: {e}", tmp.display()))?;
    // re-read: the user may have edited meanwhile
    let Some(mut profile) = lib.get_profile(pid)? else {
        let _ = std::fs::remove_file(&tmp);
        return Ok(Preview::Nothing);
    };
    if !same_key(&preview_key(r.profiles_dir, &profile)?, &key) {
        let _ = std::fs::remove_file(&tmp);
        return Ok(Preview::Stale); // never publish a preview made from an older voice setting
    }
    std::fs::rename(&tmp, pdir.join("preview.wav")).map_err(|e| e.to_string())?;
    let created = (r.clock)();
    profile.insert("preview".into(), json!({"file": "preview.wav", "engine": engine, "variant": key.0, "created": created,
                                             "duration": py_round(padded.len() as f64 / sr as f64, 2)}));
    (r.put_profile)(&mut profile)?;
    Ok(Preview::Saved)
}

#[cfg(test)]
mod tests {
    use super::beats;

    #[test]
    fn a_clean_take_beats_a_failed_take_with_a_better_score() {
        // test_audit_fixes.py: the cut-off take scored 2.5, the clean one 3.0
        assert!(beats((false, 3.0), (true, 2.5)));
        assert!(!beats((true, 2.5), (false, 3.0)));
        assert!(beats((true, 1.0), (true, 2.5)) && !beats((false, 3.0), (false, 3.0)));
    }
}
