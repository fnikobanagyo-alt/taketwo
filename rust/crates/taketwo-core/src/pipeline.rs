//! The parts of backend/app/pipeline.py that start no render (step S07a): the new narration document
//! (`create_generation`, without the save) and the check of a re-spoken part's text (`clean_replacement`).
//! Step S11b adds the final-mix edge check `processed_edge_issues` (`_processed_edge_issues`, used by `final_scan`).

use crate::library::Doc;
use crate::text::{self, Pronunciation, py};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// delivery.py DEFAULT_VOICE_SETTINGS, same keys and order.
pub fn default_voice_settings() -> Doc {
    match json!({"stability": 0.5, "similarity": 0.75, "style": 0.0, "mic_match": true, "studio_finish": true}) {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

/// The settings a narration keeps with it, in pipeline.py's order.
pub const KEPT_SETTINGS: [&str; 10] = [
    "max_chunk_chars", "max_attempts", "compare_takes", "max_wer", "min_speaker_similarity", "max_internal_silence",
    "sentence_pause_ms", "paragraph_pause_ms", "target_lufs", "seed",
];

pub const EMPTY_SCRIPT: &str = "Nothing to say: the script is empty after cleanup.";

/// Why a pipeline function refused: `Value` is Python's ValueError (the API answers 400 with it);
/// `Other` is any other exception (500).
#[derive(Debug, PartialEq)]
pub enum Fail {
    Value(String),
    Other(String),
}

pub fn pronunciations(s: &Doc) -> Vec<Pronunciation> {
    let field = |p: &Value, k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    s.get("pronunciations")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|p| Pronunciation { from: field(p, "from"), to: field(p, "to") }).collect())
        .unwrap_or_default()
}

/// `clean_text(text, s.get("pronunciations"), s.get("strip_citations", True))`.
pub fn clean_with_settings(text: &str, s: &Doc) -> String {
    text::clean_text(text, &pronunciations(s), s.get("strip_citations").is_none_or(py::truthy))
}

/// What main.py passes to `pipeline.create_generation`, already resolved (engine, language).
pub struct NewNarration<'a> {
    pub profile: &'a Doc,
    pub text: &'a str,
    pub engine: &'a str,
    pub language: Value,
    pub params: Value,
    pub speed: Option<f64>,
    pub voice_settings: Option<&'a Doc>,
    pub title: Option<&'a str>,
    pub source: &'a str,
}

fn key<'a>(d: &'a Doc, k: &str) -> Result<&'a Value, Fail> {
    d.get(k).ok_or_else(|| Fail::Other(format!("KeyError: {k}")))
}

/// pipeline.create_generation without the save: the document `db.put_generation` would store, minus the
/// `created` and `updated` stamps the save adds. `gid` replaces Python's `uuid4().hex[:12]`.
pub fn create_generation(gid: &str, req: &NewNarration, s: &Doc) -> Result<Doc, Fail> {
    let cleaned = clean_with_settings(req.text, s);
    let max_chars = py::int(key(s, "max_chunk_chars")?).map_err(Fail::Other)?;
    let chunks = text::chunk_text(&cleaned, usize::try_from(max_chars).unwrap_or(0));
    if chunks.is_empty() {
        return Err(Fail::Value(EMPTY_SCRIPT.into()));
    }
    let speed = match req.speed {
        Some(x) if x != 0.0 => x,
        _ => 1.0,
    };
    let mut vs = default_voice_settings();
    for (k, v) in req.voice_settings.into_iter().flatten() {
        if let Some(slot) = vs.get_mut(k) {
            *slot = v.clone();
        }
    }
    let title: String = py::strip(req.title.unwrap_or("")).chars().take(120).collect();
    let title = if title.is_empty() {
        let words = py::split(&cleaned);
        words.iter().take(8).copied().collect::<Vec<_>>().join(" ") + if words.len() > 8 { "…" } else { "" }
    } else {
        title
    };
    let source = if ["app", "mcp", "api"].contains(&req.source) { req.source } else { "api" };
    let chunks: Vec<Value> = chunks
        .iter()
        .enumerate()
        .map(|(i, c)| json!({"i": i, "text": c.text, "pause_after": c.pause_after, "status": "pending"}))
        .collect();
    let mut settings = Doc::new();
    for k in KEPT_SETTINGS {
        settings.insert(k.into(), key(s, k)?.clone());
    }
    let mut g = Doc::new();
    g.insert("id".into(), gid.into());
    g.insert("profile_id".into(), key(req.profile, "id")?.clone());
    g.insert("profile_name".into(), key(req.profile, "name")?.clone());
    g.insert("engine".into(), req.engine.into());
    g.insert("language".into(), req.language.clone());
    g.insert("params".into(), req.params.clone());
    g.insert("speed".into(), speed.clamp(0.7, 1.3).into()); // max(0.7, min(1.3, x)); NaN is refused by the body check
    g.insert("favorite".into(), false.into());
    g.insert("voice_settings".into(), Value::Object(vs));
    g.insert("title".into(), title.into());
    g.insert("source".into(), source.into());
    g.insert("text".into(), req.text.into());
    g.insert("cleaned_text".into(), cleaned.into());
    g.insert("status".into(), "queued".into());
    g.insert("progress".into(), 0.0.into());
    g.insert("message".into(), "Queued".into());
    g.insert("chunks".into(), chunks.into());
    g.insert("settings".into(), Value::Object(settings));
    Ok(g)
}

/// pipeline.clean_replacement: the cleaned text for a re-spoken part, or why it can't be one part.
pub fn clean_replacement(text: &str, doc: &Doc, s: &Doc) -> Result<String, Fail> {
    let cleaned = clean_with_settings(text, s);
    if py::strip(&cleaned).is_empty() {
        return Err(Fail::Value("The new text is empty after cleanup.".into()));
    }
    let kept = doc.get("settings").filter(|v| py::truthy(v)).and_then(|v| v.get("max_chunk_chars")).filter(|v| py::truthy(v));
    let limit = match kept {
        Some(v) => v,
        None => key(s, "max_chunk_chars")?,
    };
    let limit = py::int(limit).map_err(Fail::Other)?;
    let n = py::len(&cleaned) as i64;
    if n > limit {
        return Err(Fail::Value(format!(
            "This part would be {n} characters; a part can be at most {limit}. Split the text into two parts."
        )));
    }
    Ok(cleaned)
}

/// Python's `f"{n:,}"` for an integer.
pub fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// `_processed_edge_issues(reference, processed, sr)`: speech missing at the edges of a processed part. Compares
/// 10 ms frame levels; output within 5 frames counts as present; only a loss of 4 frames or more within 40 frames
/// of the first or last expected frame is flagged. Same timeline and rate on both sides.
pub fn processed_edge_issues(reference: &[f32], processed: &[f32], sr: u32) -> BTreeSet<&'static str> {
    let peak = |a: &[f32]| a.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if reference.is_empty() || peak(reference) < 1e-5f32 {
        return BTreeSet::new();
    }
    if processed.is_empty() || peak(processed) < 1e-5f32 {
        return BTreeSet::from(["processed_audio_missing"]);
    }
    let mut padded = processed[..processed.len().min(reference.len())].to_vec();
    padded.resize(reference.len(), 0.0);
    let rf = crate::audio::frame_db(reference, sr, 10);
    let got = crate::audio::frame_db(&padded, sr, 10);
    let fmax = |v: &[f32]| v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let (ref_top, got_top) = (fmax(&rf), fmax(&got));
    let nearby: Vec<f32> = (0..got.len())
        .map(|i| {
            let lo = i as i64 - 5;
            let w = &got[lo.max(0) as usize..(i + 6).min(got.len())];
            // frames past either end are the -120 pad
            if lo < 0 || i + 6 > got.len() { fmax(w).max(-120.0) } else { fmax(w) }
        })
        .collect();
    let expected: Vec<bool> = rf.iter().map(|&d| d > ref_top - 35.0f32).collect();
    let live: Vec<i64> = (0..expected.len()).filter(|&i| expected[i]).map(|i| i as i64).collect();
    let mut run = 0;
    for i in 0..expected.len().min(nearby.len()) {
        let missing = expected[i] && nearby[i] < got_top - 60.0f32;
        let i = i as i64;
        let at_edge = !live.is_empty() && (i <= live[0] + 40 || i >= live[live.len() - 1] - 40);
        run = if at_edge && missing { run + 1 } else { 0 };
        if run >= 4 {
            return BTreeSet::from(["processed_audio_cut_off"]);
        }
    }
    BTreeSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_like_python() {
        for (n, want) in [(0, "0"), (999, "999"), (1000, "1,000"), (10_001, "10,001"), (1_234_567, "1,234,567"), (-1000, "-1,000")] {
            assert_eq!(thousands(n), want);
        }
    }
}
