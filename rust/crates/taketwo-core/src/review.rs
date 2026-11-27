//! Review rules and take bookkeeping, ported from backend/app/pipeline.py (`summarize` to `take_audio`).
//! Where Python would raise (a missing key, an index out of range) these return `Err`.

use crate::library::Doc;
use crate::text::fix_entities;
use crate::text::py::{self, truthy};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Plain-English reasons for the API and MCP (must match ISSUE_LABEL in frontend/src/api.ts).
pub const ISSUE_LABEL: [(&str, &str); 25] = [
    ("cut_off", "Cut-off ending"),
    ("lead_burst", "Stray sound at the start"),
    ("end_word_missing", "Last word missing"),
    ("start_word_missing", "First word missing"),
    ("processed_audio_missing", "Audio missing from final mix"),
    ("processed_audio_cut_off", "Speech lost in final mix"),
    ("ending_unverified", "Listen to sentence ending"),
    ("odd_pause", "Odd pause"),
    ("leak", "Stray next word"),
    ("junk_tail", "Stray sound at the end"),
    ("flat", "Too monotone"),
    ("off_pitch", "Pitch off from the voice"),
    ("long_silence", "Silence gap"),
    ("too_long", "Too long / looping"),
    ("too_short", "Cut short"),
    ("voice_drift", "Voice drift"),
    ("noise", "Noise burst"),
    ("wrong_words", "Wrong words"),
    ("mispronounced", "Mispronounced"),
    ("clipping", "Clipping"),
    ("empty", "No audio"),
    ("trimmed_tail", "Tail trimmed"),
    ("trimmed_head", "Head trimmed"),
    ("final_check", "Failed the final check"),
    ("web_artifact", "Web code in the script (such as &#39;)"),
];

/// What an archived take keeps ("Sounds fine" is not carried across a take switch).
pub const TAKE_FIELDS: [&str; 15] = [
    "text", "file", "raw_file", "context_file", "wer", "sim", "dur", "issues", "final_issues", "heard", "pitch_hz", "words",
    "ending", "status", "chosen_seed",
];
pub const MAX_OLD_TAKES: usize = 3;

fn key<'a>(d: &'a Doc, k: &str) -> Result<&'a Value, String> {
    d.get(k).ok_or_else(|| format!("KeyError: {k:?}"))
}

fn obj(v: &Value) -> Result<&Doc, String> {
    v.as_object().ok_or_else(|| format!("TypeError: not a dict: {v}"))
}

/// `d.get(k) or []` as a list.
fn list_or_empty<'a>(d: &'a Doc, k: &str) -> Result<&'a [Value], String> {
    match d.get(k).filter(|v| truthy(v)) {
        None => Ok(&[]),
        Some(v) => v.as_array().map(Vec::as_slice).ok_or_else(|| format!("TypeError: {k} is not a list")),
    }
}

/// `c.get("text") or ""`.
fn text_or_empty(c: &Doc) -> Result<&str, String> {
    match c.get("text").filter(|v| truthy(v)) {
        None => Ok(""),
        Some(v) => v.as_str().ok_or_else(|| "TypeError: text is not a str".into()),
    }
}

fn has_web_artifact(text: &str) -> bool {
    fix_entities(text) != text
}

/// `list[i]` with Python's negative indexing.
fn position(len: usize, i: i64) -> Result<usize, String> {
    let p = if i < 0 { i + len as i64 } else { i };
    if p < 0 || p >= len as i64 { Err(format!("IndexError: list index {i} out of range")) } else { Ok(p as usize) }
}

fn chunks(doc: &Doc) -> Result<&Vec<Value>, String> {
    key(doc, "chunks")?.as_array().ok_or_else(|| "TypeError: chunks is not a list".into())
}

fn chunk(doc: &Doc, i: i64) -> Result<&Doc, String> {
    let c = chunks(doc)?;
    obj(&c[position(c.len(), i)?])
}

fn chunk_mut(doc: &mut Doc, i: i64) -> Result<&mut Doc, String> {
    let c = doc.get_mut("chunks").and_then(Value::as_array_mut).ok_or("KeyError: \"chunks\"")?;
    let p = position(c.len(), i)?;
    c[p].as_object_mut().ok_or_else(|| "TypeError: chunk is not a dict".into())
}

fn index(v: &Value) -> Result<i64, String> {
    v.as_i64().ok_or_else(|| format!("TypeError: part index {v} is not an int"))
}

/// Parts that need a re-take: flagged, failing the final check, or carrying a web artifact; a part
/// a person marked "Sounds fine" is left out.
pub fn fix_targets(doc: &Doc) -> Result<Vec<i64>, String> {
    let mut out = BTreeSet::new();
    if let Some(scan) = doc.get("final_scan").filter(|v| truthy(v)) {
        for i in list_or_empty(obj(scan)?, "remaining")? {
            out.insert(index(i)?);
        }
    }
    let all: &[Value] = match doc.get("chunks") {
        None => &[],
        Some(v) => v.as_array().ok_or("TypeError: chunks is not a list")?,
    };
    let mut by_i = HashMap::new();
    for c in all {
        let c = obj(c)?;
        let i = key(c, "i")?;
        if c.get("status").and_then(Value::as_str) == Some("flagged") {
            out.insert(index(i)?);
        }
        if has_web_artifact(text_or_empty(c)?) {
            out.insert(index(i)?);
        }
        by_i.insert(i.to_string(), c);
    }
    Ok(out.into_iter().filter(|i| !by_i.get(&i.to_string()).and_then(|c| c.get("accepted")).is_some_and(truthy)).collect())
}

fn label(code: &str) -> String {
    ISSUE_LABEL.iter().find(|(c, _)| *c == code).map(|(_, l)| l.to_string()).unwrap_or_else(|| code.replace('_', " "))
}

/// Parts a person should listen to: the parts the Fix button re-records, with text, reasons,
/// where they sit in the finished audio and what was measured.
pub fn review(doc: &Doc, voice_pitch_hz: Option<f64>) -> Result<Vec<Value>, String> {
    let mut spans: HashMap<String, &Doc> = HashMap::new();
    for x in list_or_empty(doc, "timeline")? {
        let x = obj(x)?;
        spans.insert(key(x, "i")?.to_string(), x);
    }
    let mut out = Vec::new();
    for i in fix_targets(doc)? {
        let ch = chunk(doc, i)?;
        let (issues, finals) = (list_or_empty(ch, "issues")?, list_or_empty(ch, "final_issues")?);
        let mut codes: Vec<Value> = Vec::new();
        for c in issues.iter().chain(finals) {
            if !codes.contains(c) {
                codes.push(c.clone());
            }
        }
        let text = text_or_empty(ch)?;
        if has_web_artifact(text) {
            codes.push("web_artifact".into());
        }
        if codes.is_empty() {
            codes.push("final_check".into());
        }
        let script = codes.iter().any(|c| c == "web_artifact");
        let mut found_by: Vec<&str> = [("take check", !issues.is_empty()), ("final check", !finals.is_empty()), ("script check", script)]
            .iter()
            .filter(|x| x.1)
            .map(|x| x.0)
            .collect();
        if found_by.is_empty() {
            found_by.push("final check");
        }
        let reasons = codes
            .iter()
            .map(|c| c.as_str().map(label).ok_or_else(|| format!("AttributeError: issue code {c} is not a str")))
            .collect::<Result<Vec<_>, _>>()?;
        let span = spans.get(&i.to_string());
        let at = |k: &str| span.and_then(|s| s.get(k)).cloned().unwrap_or(Value::Null);
        let heard = ch.get("heard").filter(|v| truthy(v)).cloned().unwrap_or_else(|| "".into());
        let mut item = Map::new();
        item.insert("part".into(), (i + 1).into());
        item.insert("text".into(), text.into());
        item.insert("issues".into(), codes.into());
        item.insert("reasons".into(), reasons.into());
        item.insert("start".into(), at("start"));
        item.insert("end".into(), at("end"));
        item.insert("heard".into(), heard);
        item.insert("pitch_hz".into(), ch.get("pitch_hz").cloned().unwrap_or(Value::Null));
        item.insert("voice_pitch_hz".into(), voice_pitch_hz.map(Value::from).unwrap_or(Value::Null));
        item.insert("checked_by".into(), found_by.into());
        item.insert("take".into(), ch.get("take").cloned().unwrap_or(1.into()));
        out.push(item.into());
    }
    Ok(out)
}

/// The finished narration's flagged list and one-line message, from its parts as they are now.
pub fn summarize(doc: &mut Doc) -> Result<(), String> {
    let mut flagged = Vec::new();
    let mut heard = 0;
    for c in chunks(doc)? {
        let c = obj(c)?;
        let accepted = c.get("accepted").is_some_and(truthy);
        if c.get("status").and_then(Value::as_str) == Some("flagged") && !accepted {
            flagged.push(key(c, "i")?.clone());
        }
        heard += accepted as usize;
    }
    doc.insert("flagged".into(), flagged.into());
    let todo = review(doc, None)?;
    let message = if !todo.is_empty() {
        let parts: Vec<String> = todo.iter().map(|r| r["part"].to_string()).collect();
        format!("Done, needs review: part {}.", parts.join(", "))
    } else if heard > 0 {
        let s = if heard > 1 { "s" } else { "" };
        format!("Done. You marked {heard} part{s} as sounding fine; every other part passed the checks.")
    } else {
        "Done. All parts passed the automated checks.".to_string()
    };
    doc.insert("message".into(), message.into());
    Ok(())
}

/// "clean" or "needs_listen" once a narration is done; None before.
pub fn outcome(doc: &Doc) -> Result<Option<&'static str>, String> {
    if doc.get("status").and_then(Value::as_str) != Some("done") {
        return Ok(None);
    }
    Ok(Some(if fix_targets(doc)?.is_empty() { "clean" } else { "needs_listen" }))
}

/// Mark one part "Sounds fine" (a person listened and keeps it) or undo that. Caller saves.
pub fn set_accepted(doc: &mut Doc, ci: i64, on: bool, clock: &dyn Fn() -> f64) -> Result<(), String> {
    let accepted = if on { json!({"by": "person", "at": clock()}) } else { Value::Null };
    chunk_mut(doc, ci)?.insert("accepted".into(), accepted);
    if doc.get("status").and_then(Value::as_str) == Some("done") {
        summarize(doc)?;
    }
    Ok(())
}

pub fn take_dir(generations: &Path, gid: &str, ci: i64, k: &str) -> PathBuf {
    generations.join(gid).join("takes").join(format!("{ci:03}-{k}"))
}

/// `ch.get(name)` when it is a non-empty file name.
fn file_name<'a>(ch: &'a Doc, name: &str) -> Result<Option<&'a str>, String> {
    match ch.get(name).filter(|v| truthy(v)) {
        None => Ok(None),
        Some(v) => v.as_str().map(Some).ok_or_else(|| format!("TypeError: {name} is not a str")),
    }
}

/// `shutil.copy2`: contents, permissions and times.
pub fn copy2(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::copy(src, dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    let m = std::fs::metadata(src).map_err(|e| e.to_string())?;
    let times = std::fs::FileTimes::new()
        .set_accessed(m.accessed().map_err(|e| e.to_string())?)
        .set_modified(m.modified().map_err(|e| e.to_string())?);
    std::fs::File::options().write(true).open(dst).and_then(|f| f.set_times(times)).map_err(|e| e.to_string())
}

/// Copy the part's kept take (audio and what was measured) aside, so it can be heard and chosen again.
pub fn archive_take(generations: &Path, gid: &str, ch: &Doc, clock: &dyn Fn() -> f64) -> Result<Option<Doc>, String> {
    let gdir = generations.join(gid);
    match file_name(ch, "file")? {
        Some(f) if gdir.join(f).exists() => {}
        _ => return Ok(None),
    }
    let k = py::int(ch.get("take").unwrap_or(&1.into()))?;
    let d = take_dir(generations, gid, index(key(ch, "i")?)?, &k.to_string());
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    for name in ["file", "raw_file", "context_file"] {
        if let Some(f) = file_name(ch, name)?
            && gdir.join(f).exists()
        {
            copy2(&gdir.join(f), &d.join(f))?;
        }
    }
    let mut take: Doc = TAKE_FIELDS.iter().map(|f| (f.to_string(), ch.get(*f).cloned().unwrap_or(Value::Null))).collect();
    take.insert("take".into(), k.into());
    take.insert("archived".into(), clock().into());
    Ok(Some(take))
}

/// Keep the newest `MAX_OLD_TAKES` archived takes; remove the older folders.
pub fn trim_takes(generations: &Path, gid: &str, ch: &mut Doc) -> Result<(), String> {
    let takes: Vec<Value> = match ch.get("takes") {
        None => Vec::new(),
        Some(v) => v.as_array().cloned().ok_or("TypeError: takes is not a list")?,
    };
    let cut = takes.len().saturating_sub(MAX_OLD_TAKES);
    for old in &takes[..cut] {
        let k = py::display(key(obj(old)?, "take")?);
        let _ = std::fs::remove_dir_all(take_dir(generations, gid, index(key(ch, "i")?)?, &k)); // ignore_errors=True
    }
    ch.insert("takes".into(), takes[cut..].to_vec().into());
    Ok(())
}

/// The audio file of take `k` of part `ci`: the current one or an archived one; None when missing.
pub fn take_audio(generations: &Path, doc: &Doc, ci: i64, k: i64) -> Result<Option<PathBuf>, String> {
    let ch = chunk(doc, ci)?;
    let id = key(doc, "id")?.as_str().ok_or("TypeError: id is not a str")?;
    let name = format!("chunk_{ci:03}.wav");
    let f = if k == py::int(ch.get("take").unwrap_or(&1.into()))? {
        generations.join(id).join(name)
    } else {
        take_dir(generations, id, ci, &k.to_string()).join(name)
    };
    Ok(f.exists().then_some(f))
}
