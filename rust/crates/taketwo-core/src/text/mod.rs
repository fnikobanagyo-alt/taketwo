//! Script cleanup and sentence-aware chunking: a port of `backend/app/textproc.py`, plus the
//! Whisper text normalizer, WER and word coverage used by the checks.
//! Lengths are in code points, as in Python.

mod html;
pub mod normalize;
pub mod py;
pub mod score;

pub use normalize::normalize_for_wer;
pub use score::{final_word_missing, first_word_missing, ratio, wer, word_coverage, SequenceMatcher};

use regex::Regex;
use std::sync::LazyLock;

pub const ABBREVIATIONS: [&str; 23] = [
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "no", "vol", "fig", "approx",
    "dept", "est", "inc", "ltd", "co", "mt", "ft",
];

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| py::re($pat));
    };
}

re!(BARE_NUMERIC_ENTITY, r"&(\d{2,5});");
re!(CITATION, r"[{SPACE}]*\[\d+(?:[,\-–][{SPACE}]*\d+)*\]");
re!(DASH, r"[{SPACE}]*[—–][{SPACE}]*");
re!(MARKDOWN, r"[*_#`>|~]");
re!(URL, r"https?://[^{SPACE}]+");
re!(SPACES_TABS, r"[ \t]+");
re!(LINE_EDGE, r" ?\n ?");
re!(MANY_NEWLINES, r"\n{3,}");
re!(DOUBLE_COMMA, r",[{SPACE}]*,");
re!(LEADING_COMMA, r"(^|\n),[{SPACE}]*");
re!(BLANK_LINE, r"\n[{SPACE}]*\n");
re!(PARAGRAPH, r"\n[{SPACE}]*\n|\n");
re!(CJK, r"[\x{3040}-\x{30ff}\x{3400}-\x{4dbf}\x{4e00}-\x{9fff}\x{ac00}-\x{d7af}\x{f900}-\x{faff}]");
re!(WORDS, r"[{WORD}']+");

static SENTENCE_END: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    fancy_regex::Regex::new(&format!(r#"(?<=[.!?])["')\]]?[{}]+(?=["'(\[]?[A-Za-z0-9])"#, py::SPACE)).unwrap()
});

/// Undo web/HTML artifacts in a pasted script: &39; &amp; &quot; &nbsp; ...
/// Left in place, the voice reads them aloud ("Marika and 39's").
pub fn fix_entities(text: &str) -> String {
    let t = BARE_NUMERIC_ENTITY.replace_all(text, |c: &regex::Captures| {
        let n = c[1].chars().fold(0u32, |n, d| n * 10 + py::digit_value(d));
        match char::from_u32(n) {
            Some(ch) if (32..0x2000).contains(&n) => ch.to_string(),
            _ => c[0].to_string(),
        }
    });
    html::unescape(&t).replace('\u{a0}', " ")
}

/// One pronunciation rule; a missing `from` or `to` is an empty string.
#[derive(Clone, Debug, Default)]
pub struct Pronunciation {
    pub from: String,
    pub to: String,
}

pub fn clean_text(text: &str, pronunciations: &[Pronunciation], strip_citations: bool) -> String {
    let mut t = fix_entities(text).replace("\r\n", "\n").replace('\r', "\n");
    if strip_citations {
        t = CITATION.replace_all(&t, "").into_owned(); // [1], [2,3], [4-6]
    }
    t = t.replace('’', "'").replace('‘', "'").replace('“', "\"").replace('”', "\"");
    t = DASH.replace_all(&t, ", ").into_owned(); // em/en dash -> comma pause
    t = t.replace('…', "...");
    t = MARKDOWN.replace_all(&t, "").into_owned();
    t = URL.replace_all(&t, "").into_owned();
    for p in pronunciations {
        let (src, dst) = (py::strip(&p.from), py::strip(&p.to));
        if !src.is_empty() && !dst.is_empty() {
            // shortcut: `to` is inserted literally; Python reads it as a re.sub template
            // (backslash escapes, and `\1` raises). Upgrade if a rule ever holds a backslash.
            let w = py::WORD;
            let pat = format!(r"(?i)(?<![{w}]){}(?![{w}])", fancy_regex::escape(src));
            let re = fancy_regex::Regex::new(&pat).expect("escaped pattern");
            t = re.replace_all(&t, |_: &fancy_regex::Captures| dst.to_string()).into_owned();
        }
    }
    t = SPACES_TABS.replace_all(&t, " ").into_owned();
    t = LINE_EDGE.replace_all(&t, "\n").into_owned();
    t = unwrap_lines(&t);
    t = MANY_NEWLINES.replace_all(&t, "\n\n").into_owned();
    t = DOUBLE_COMMA.replace_all(&t, ",").into_owned();
    t = LEADING_COMMA.replace_all(&t, "${1}").into_owned();
    py::strip(&t).to_string()
}

const TERMINAL: [char; 9] = ['.', '!', '?', ':', ';', '"', '\'', ')', ']'];

/// Rejoin lines that were hard-wrapped mid-sentence (text copied from PDFs, Docs, emails).
/// A single line break only counts as a paragraph when the line actually ends a sentence;
/// otherwise it's a wrap and becomes a space. Blank lines always stay paragraphs, and short
/// title-like lines (headings) keep their own line.
pub fn unwrap_lines(t: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for block in BLANK_LINE.split(t) {
        let lines = block.split('\n').map(py::strip).filter(|l| !l.is_empty());
        let mut merged = String::new();
        for ln in lines {
            if merged.is_empty() {
                merged = ln.to_string();
                continue;
            }
            let prev = merged.rsplit('\n').next().unwrap_or("");
            let prev_r = py::rstrip(prev);
            let ends_sentence = prev_r.ends_with(TERMINAL);
            let first = ln.chars().next();
            let heading = py::len(prev) < 50
                && first.is_some_and(char::is_uppercase)
                && !prev_r.ends_with([',', '-']);
            if ends_sentence || heading {
                merged.push('\n');
                merged.push_str(ln);
            } else if prev.ends_with('-') && first.is_some_and(char::is_lowercase) {
                merged.pop(); // "long-" + "house" -> "longhouse"
                merged.push_str(ln);
            } else if prev.ends_with('-') {
                merged.push_str(ln); // "well-" + "Known" -> "well-Known"
            } else {
                merged.push(' ');
                merged.push_str(ln);
            }
        }
        out.push(merged);
    }
    out.retain(|o| !o.is_empty());
    out.join("\n\n")
}

/// Python's `re.findall(r"(\b[\w.]+)\.$", s)[0]`, if any.
fn word_before_final_period(s: &str) -> Option<String> {
    let body = s.strip_suffix('\n').unwrap_or(s); // `$` also matches before a final newline
    let c: Vec<char> = body.chars().collect();
    let n = c.len();
    if n < 2 || c[n - 1] != '.' {
        return None;
    }
    let mut q = n;
    while q > 0 && (py::is_word(c[q - 1]) || c[q - 1] == '.') {
        q -= 1;
    }
    (q..n - 1).find(|&p| py::boundary(&c, p)).map(|p| c[p..n - 1].iter().collect())
}

pub fn split_sentences(paragraph: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut last = 0;
    for m in SENTENCE_END.find_iter(paragraph) {
        let m = m.expect("sentence regex");
        parts.push(&paragraph[last..m.start()]);
        last = m.end();
    }
    parts.push(&paragraph[last..]);
    // Re-join false splits after abbreviations / initials.
    let mut out: Vec<String> = Vec::new();
    for p in parts {
        if let Some(prev) = out.last_mut()
            && let Some(w) = word_before_final_period(prev)
            && (ABBREVIATIONS.contains(&w.to_lowercase().as_str()) || is_initial(&w))
        {
            prev.push(' ');
            prev.push_str(p);
            continue;
        }
        out.push(py::strip(p).to_string());
    }
    out.retain(|s| !s.is_empty());
    out
}

fn is_initial(w: &str) -> bool {
    let mut c = w.chars();
    matches!((c.next(), c.next()), (Some(ch), None) if ch.is_ascii_uppercase())
}

/// Soft break points, including CJK punctuation.
const SOFT_BREAKS: [char; 12] =
    [',', ';', ':', '.', '!', '?', '\u{3001}', '\u{3002}', '\u{ff01}', '\u{ff0c}', '\u{ff1b}', '\u{ff1f}'];

/// Cut a run with no spaces (CJK text, a long URL or code token) into pieces of at most
/// max_chars, preferring a break right after punctuation in the second half of the window.
fn hard_split(token: &str, max_chars: usize) -> Vec<String> {
    let mut token: Vec<char> = token.chars().collect();
    let mut out = Vec::new();
    while token.len() > max_chars {
        let window = &token[..max_chars];
        let cut = (max_chars / 2 + 1..=window.len())
            .rev()
            .find(|&j| SOFT_BREAKS.contains(&window[j - 1]))
            .unwrap_or(max_chars);
        out.push(token[..cut].iter().collect());
        token.drain(..cut);
    }
    if !token.is_empty() {
        out.push(token.iter().collect());
    }
    out
}

/// Python's `re.split(r"(?<=[,;:])\s+", s)`.
fn split_clauses(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut prev) = (0, None);
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if py::is_space(c) && matches!(prev, Some(',' | ';' | ':')) {
            out.push(&s[start..i]);
            while let Some(&(_, c2)) = it.peek() {
                if !py::is_space(c2) {
                    break;
                }
                it.next();
            }
            start = it.peek().map_or(s.len(), |&(j, _)| j);
            prev = None;
            continue;
        }
        prev = Some(c);
    }
    out.push(&s[start..]);
    out
}

fn join_strip(a: &str, b: &str) -> String {
    py::strip(&format!("{a} {b}")).to_string()
}

/// Split an over-long sentence at clause punctuation, then at spaces, then by characters.
fn split_long(sentence: &str, max_chars: usize) -> Vec<String> {
    if py::len(sentence) <= max_chars {
        return vec![sentence.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for piece in split_clauses(sentence) {
        let mut p = piece.to_string();
        if py::len(&p) > max_chars {
            // Flush earlier clauses before emitting pieces of the next clause.
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let mut buf = String::new();
            for w in py::split(&p) {
                if py::len(w) > max_chars {
                    // no spaces to break at: split the run itself
                    if !buf.is_empty() {
                        out.push(buf.clone());
                    }
                    let mut pieces = hard_split(w, max_chars);
                    buf = pieces.pop().unwrap_or_default();
                    out.extend(pieces);
                    continue;
                }
                if py::len(&buf) + py::len(w) + 1 > max_chars && !buf.is_empty() {
                    out.push(std::mem::replace(&mut buf, w.to_string()));
                } else {
                    buf = join_strip(&buf, w);
                }
            }
            p = buf;
        }
        if !cur.is_empty() && py::len(&cur) + py::len(&p) + 1 > max_chars {
            out.push(std::mem::replace(&mut cur, p));
        } else {
            cur = join_strip(&cur, &p);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// "sentence" | "paragraph" | "end"
    pub pause_after: &'static str,
}

/// Group whole sentences into chunks of <= max_chars, never crossing paragraphs.
///
/// Short chunks are the single biggest reliability win for autoregressive TTS:
/// long prompts are where models drift into silence, babble or loops.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let paragraphs: Vec<&str> = PARAGRAPH.split(text).map(py::strip).filter(|p| !p.is_empty()).collect();
    for (pi, para) in paragraphs.iter().enumerate() {
        let sentences: Vec<String> =
            split_sentences(para).iter().flat_map(|s| split_long(s, max_chars)).collect();
        let mut cur = String::new();
        let mut para_chunks: Vec<String> = Vec::new();
        for s in sentences {
            if !cur.is_empty() && py::len(&cur) + py::len(&s) + 1 > max_chars {
                para_chunks.push(std::mem::replace(&mut cur, s));
            } else {
                cur = join_strip(&cur, &s);
            }
        }
        if !cur.is_empty() {
            para_chunks.push(cur);
        }
        // Avoid tiny orphan chunks (models handle 1-2 word prompts badly).
        let k = para_chunks.len();
        if k > 1
            && py::len(&para_chunks[k - 1]) < 25
            && ((py::len(&para_chunks[k - 2]) + py::len(&para_chunks[k - 1])) as f64) < max_chars as f64 * 1.3
        {
            let orphan = para_chunks.pop().unwrap();
            let last = para_chunks.last_mut().unwrap();
            last.push(' ');
            last.push_str(&orphan);
        }
        let n = para_chunks.len();
        for (ci, c) in para_chunks.into_iter().enumerate() {
            let last_in_para = ci == n - 1;
            let kind = if last_in_para && pi == paragraphs.len() - 1 {
                "end"
            } else if last_in_para {
                "paragraph"
            } else {
                "sentence"
            };
            chunks.push(Chunk { text: c, pause_after: kind });
        }
    }
    chunks
}

/// Spoken-word estimate. Each CJK/kana/Hangul character is a syllable, so a run of them
/// is many words, not one (a 200-character Chinese sentence has no spaces).
pub fn word_count(text: &str) -> usize {
    WORDS.find_iter(&CJK.replace_all(text, " ")).count() + CJK.find_iter(text).count()
}
