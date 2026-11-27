//! Subtitles and word timings, ported from backend/app/tools.py (`MAX_CAPTION_CHARS` to `to_txt`).
//! Lengths are code points. Numbers keep Python's int/float split where Python passes them through.

use crate::library::Doc;
use crate::text::py::{self, num, truthy};
use crate::text::{split_sentences, SequenceMatcher};
use serde_json::{Map, Value};
use std::collections::HashMap;

pub const MAX_CAPTION_CHARS: usize = 84; // two lines of ~42

/// Python `round(x, 3)` on a float: the exact binary value rounded half-to-even to 3 decimals, read back.
/// Rust's `{:.3}` formats the exact value with ties to even, which is what CPython's dtoa does.
pub fn round3(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.3}").parse().expect("formatted float parses")
}

/// `round(v, 3)` on a JSON number: ints (and bools) stay ints, floats are rounded.
fn round3_value(v: &Value) -> Result<Value, String> {
    match v {
        Value::Bool(b) => Ok((*b as i64).into()),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(v.clone()),
        _ => Ok(round3(num(v)?).into()),
    }
}

/// `re.split(r"(?<=[marks])\s+", text)`: split at each whitespace run that follows one of `marks`.
fn split_after<'a>(text: &'a str, marks: &[char]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let (mut start, mut prev) = (0, None::<char>);
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if py::is_space(c) && prev.is_some_and(|p| marks.contains(&p)) {
            let mut end = i + c.len_utf8();
            while let Some(&(j, d)) = it.peek() {
                if !py::is_space(d) {
                    break;
                }
                end = j + d.len_utf8();
                it.next();
            }
            out.push(&text[start..i]);
            start = end;
            prev = Some(' ');
        } else {
            prev = Some(c);
        }
    }
    out.push(&text[start..]);
    out
}

/// Sentence -> caption-sized phrases, split at clause punctuation then words.
pub fn caption_pieces(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for sent in split_after(py::strip(text), &['.', '!', '?']) {
        if py::len(sent) <= MAX_CAPTION_CHARS {
            out.push(sent.to_string());
            continue;
        }
        let mut buf = String::new();
        for part in split_after(sent, &[',', ';', ':']) {
            for w in py::split(part) {
                if !buf.is_empty() && py::len(&buf) + py::len(w) + 1 > MAX_CAPTION_CHARS {
                    out.push(std::mem::replace(&mut buf, w.to_string()));
                } else {
                    buf = py::strip(&format!("{buf} {w}")).to_string();
                }
            }
            if py::len(&buf) as f64 > MAX_CAPTION_CHARS as f64 * 0.55 {
                out.push(std::mem::take(&mut buf));
            }
        }
        if !buf.is_empty() {
            out.push(buf);
        }
    }
    out.retain(|o| !o.is_empty());
    out
}

fn key<'a>(d: &'a Doc, k: &str) -> Result<&'a Value, String> {
    d.get(k).ok_or_else(|| format!("KeyError: {k:?}"))
}

fn obj(v: &Value) -> Result<&Doc, String> {
    v.as_object().ok_or_else(|| format!("TypeError: not a dict: {v}"))
}

fn list<'a>(v: &'a Value, what: &str) -> Result<&'a Vec<Value>, String> {
    v.as_array().ok_or_else(|| format!("TypeError: {what} is not a list"))
}

fn text_str(d: &Doc) -> Result<&str, String> {
    key(d, "text")?.as_str().ok_or_else(|| "AttributeError: text is not a str".into())
}

/// `{c["i"]: c for c in gen["chunks"]}` (the last duplicate wins).
fn chunk_map(doc: &Doc) -> Result<HashMap<String, &Doc>, String> {
    let mut m = HashMap::new();
    for c in list(key(doc, "chunks")?, "chunks")? {
        let c = obj(c)?;
        m.insert(key(c, "i")?.to_string(), c);
    }
    Ok(m)
}

/// `gen.get("timeline", [])`: missing is empty; a stored null raises in Python.
fn timeline(doc: &Doc) -> Result<Vec<&Doc>, String> {
    match doc.get("timeline") {
        None => Ok(Vec::new()),
        Some(v) => list(v, "timeline")?.iter().map(obj).collect(),
    }
}

fn chunk_for<'a>(chunks: &HashMap<String, &'a Doc>, t: &Doc) -> Result<&'a Doc, String> {
    let i = key(t, "i")?;
    chunks.get(&i.to_string()).copied().ok_or_else(|| format!("KeyError: {i}"))
}

fn cap(start: Value, end: Value, text: String) -> Doc {
    let mut m = Map::new();
    m.insert("start".into(), start);
    m.insert("end".into(), end);
    m.insert("text".into(), text.into());
    m
}

/// Captions timed from each chunk's real position in the final audio; inside a chunk time is
/// shared by character count.
pub fn generation_captions(doc: &Doc) -> Result<Vec<Doc>, String> {
    let chunks = chunk_map(doc)?;
    let mut caps = Vec::new();
    for t in timeline(doc)? {
        let pieces = caption_pieces(text_str(chunk_for(&chunks, t)?)?);
        let total = pieces.iter().map(|p| py::len(p)).sum::<usize>().max(1);
        let start = key(t, "start")?;
        let span = num(key(t, "end")?)? - num(start)?;
        let (mut cur_v, mut cur) = (start.clone(), num(start)?);
        for p in pieces {
            let d = span * py::len(&p) as f64 / total as f64;
            let end = cur + d;
            caps.push(cap(cur_v, end.into(), p));
            (cur_v, cur) = (end.into(), end);
        }
    }
    Ok(caps)
}

/// `"".join(c for c in w.lower() if c.isalnum() or c == "'")`.
fn norm(w: &str) -> String {
    w.to_lowercase().chars().filter(|&c| (py::is_word(c) && c != '_') || c == '\'').collect()
}

/// Python `float(v)` for `gen.get("speed") or 1.0`.
fn speed_of(doc: &Doc) -> Result<f64, String> {
    match doc.get("speed").filter(|v| truthy(v)) {
        None => Ok(1.0),
        Some(Value::String(s)) => py::strip(s).parse().map_err(|_| format!("ValueError: float({s:?})")),
        Some(v) => num(v),
    }
}

fn word(w: &str, start: Value, end: Value) -> Doc {
    let mut m = Map::new();
    m.insert("word".into(), w.into());
    m.insert("start".into(), start);
    m.insert("end".into(), end);
    m
}

/// Start/end of every sentence and script word in the final file (after speed).
pub fn generation_timings(doc: &Doc) -> Result<Doc, String> {
    let speed = speed_of(doc)?;
    let chunks = chunk_map(doc)?;
    let (mut words, mut sentences, mut estimated) = (Vec::<Value>::new(), Vec::<Value>::new(), false);
    for t in timeline(doc)? {
        let ch = chunk_for(&chunks, t)?;
        let text = text_str(ch)?;
        let script = py::split(text);
        let t_start = key(t, "start")?;
        let mut span: Vec<(Value, Value)> = Vec::new();
        match ch.get("words").filter(|v| truthy(v)) {
            Some(heard) => {
                let heard = list(heard, "words")?.iter().map(obj).collect::<Result<Vec<_>, _>>()?;
                let a: Vec<String> = script.iter().map(|w| norm(w)).collect();
                let b = heard
                    .iter()
                    .map(|w| key(w, "word")?.as_str().map(norm).ok_or_else(|| "AttributeError: word".to_string()))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut times: Vec<Option<(Value, Value)>> = vec![None; script.len()];
                for (i, j, size) in SequenceMatcher::new(&a, &b, false).get_matching_blocks() {
                    for k in 0..size {
                        times[i + k] = Some((key(heard[j + k], "start")?.clone(), key(heard[j + k], "end")?.clone()));
                    }
                }
                for i in 0..script.len() {
                    if times[i].is_none() {
                        let prev = (0..i).rev().find_map(|p| times[p].as_ref().map(|x| x.1.clone())).unwrap_or(0.0.into());
                        let nxt = (i + 1..script.len()).find_map(|n| times[n].as_ref().map(|x| x.0.clone())).unwrap_or(prev.clone());
                        let hi = if num(&nxt)? > num(&prev)? { nxt } else { prev.clone() }; // max(prev, nxt)
                        times[i] = Some((prev, hi));
                    }
                }
                let t0 = num(t_start)?;
                for (s, e) in times.into_iter().flatten() {
                    span.push(((t0 + num(&s)? / speed).into(), (t0 + num(&e)? / speed).into()));
                }
            }
            None => {
                estimated = true;
                let total = script.iter().map(|w| py::len(w)).sum::<usize>().max(1);
                let width = num(key(t, "end")?)? - num(t_start)?;
                let (mut cur_v, mut cur) = (t_start.clone(), num(t_start)?);
                for w in &script {
                    let d = width * py::len(w) as f64 / total as f64;
                    let end = cur + d;
                    span.push((cur_v, end.into()));
                    (cur_v, cur) = (end.into(), end);
                }
            }
        }
        let mut part = Vec::new();
        for (w, (s, e)) in script.iter().zip(&span) {
            part.push(word(w, round3_value(s)?, round3_value(e)?));
        }
        let mut k = 0;
        for sentence in split_sentences(text) {
            let n = py::split(&sentence).len();
            if n > 0 {
                let first = part.get(k).ok_or("IndexError: sentence words")?;
                let last = part.get(k + n - 1).ok_or("IndexError: sentence words")?;
                let mut m = Map::new();
                m.insert("text".into(), sentence.into());
                m.insert("start".into(), first["start"].clone());
                m.insert("end".into(), last["end"].clone());
                sentences.push(m.into());
            }
            k += n;
        }
        words.extend(part.into_iter().map(Value::Object));
    }
    let mut out = Map::new();
    out.insert("speed".into(), speed.into());
    out.insert("duration".into(), doc.get("duration").cloned().unwrap_or(Value::Null));
    out.insert("estimated".into(), estimated.into());
    out.insert("sentences".into(), sentences.into());
    out.insert("words".into(), words.into());
    Ok(out)
}

/// `_ts`: `int(round(t * 1000))` (ties to even) as `HH:MM:SS<sep>mmm`, divmod flooring as Python's.
pub fn ts(t: f64, sep: char) -> String {
    let ms = (t * 1000.0).round_ties_even() as i64;
    let (h, ms) = (ms.div_euclid(3_600_000), ms.rem_euclid(3_600_000));
    let (m, ms) = (ms.div_euclid(60_000), ms.rem_euclid(60_000));
    let (s, ms) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    format!("{h:02}:{m:02}:{s:02}{sep}{ms:03}")
}

/// `_wrap`: two lines broken at the space nearest the middle (characters when there is no space).
pub fn wrap(text: &str) -> String {
    let n = py::len(text);
    if n <= 42 {
        return text.to_string();
    }
    let (words, mid) = (py::split(text), n as f64 / 2.0);
    if words.len() < 2 {
        let cut = n / 2;
        return format!("{}\n{}", text.chars().take(cut).collect::<String>(), text.chars().skip(cut).collect::<String>());
    }
    let (mut best, mut pos, mut cut) = (0usize, 0usize, 0usize);
    for (i, w) in words[..words.len() - 1].iter().enumerate() {
        pos += py::len(w) + 1;
        if (pos as f64 - mid).abs() < (best as f64 - mid).abs() || best == 0 {
            (best, cut) = (pos, i + 1);
        }
    }
    format!("{}\n{}", words[..cut].join(" "), words[cut..].join(" "))
}

/// Content-Disposition for a download: an ASCII fallback name plus the UTF-8 `filename*`.
pub fn attachment_header(name: &str, ext: &str) -> String {
    let full = format!("{name}.{ext}");
    let ascii: String =
        name.chars().filter(char::is_ascii).map(|c| if matches!(c, '"' | '\\' | '\r' | '\n') { '_' } else { c }).collect();
    let base = match py::strip(&ascii) {
        "" => "download",
        b => b,
    };
    // urllib.parse.quote(full, safe=''): only ASCII letters, digits and `_.-~` stay as they are.
    let mut quoted = String::new();
    for b in full.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) {
            quoted.push(b as char);
        } else {
            quoted.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{base}.{ext}\"; filename*=UTF-8''{quoted}")
}

fn cue(c: &Doc, sep: char) -> Result<String, String> {
    let text = key(c, "text")?.as_str().ok_or("caption text is not a str")?;
    Ok(format!("{} --> {}\n{}\n", ts(num(key(c, "start")?)?, sep), ts(num(key(c, "end")?)?, sep), wrap(text)))
}

pub fn to_srt(caps: &[Doc]) -> Result<String, String> {
    let cues = caps.iter().enumerate().map(|(i, c)| Ok(format!("{}\n{}", i + 1, cue(c, ',')?))).collect::<Result<Vec<_>, String>>()?;
    Ok(cues.join("\n"))
}

pub fn to_vtt(caps: &[Doc]) -> Result<String, String> {
    let cues = caps.iter().map(|c| cue(c, '.')).collect::<Result<Vec<_>, String>>()?;
    Ok(format!("WEBVTT\n\n{}", cues.join("\n")))
}

pub fn to_txt(caps: &[Doc]) -> Result<String, String> {
    let texts = caps.iter().map(|c| Ok(key(c, "text")?.as_str().ok_or("caption text is not a str")?)).collect::<Result<Vec<_>, String>>()?;
    Ok(texts.join(" "))
}
